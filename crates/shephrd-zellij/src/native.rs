use serde::{Deserialize, Serialize};
use shephrd::agent_contract::{
    AgentExecution, InspectRequest, InspectResponse, PaneProcess, ProgramStatus,
    ProgramStatusUpdate, harness_name,
};
use shephrd::ci_contract::{CiRefreshTarget, CiSnapshot};
use shephrd::{
    Action, Attention, Effect, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Pane, ProjectIdentity, Request,
    Response, Snapshot, Status, Workspace,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zellij_tile::prelude::*;

pub const REQUEST: &str = "shephrd-native-request";
pub const RESPONSE: &str = "shephrd-native-response";
pub const STATE: &str = "shephrd-native-state";
pub const QUERY: &str = "shephrd-native-query";
pub const PROJECT: &str = "shephrd-native-project";
pub const REFRESH_CI: &str = "shephrd-native-refresh-ci";
#[derive(Serialize, Deserialize)]
struct NativeCiRefresh {
    client_id: u16,
    target: CiRefreshTarget,
}

#[derive(Serialize, Deserialize)]
pub struct NativeRequest {
    pub client: String,
    pub client_id: u16,
    pub request: Request,
}

#[derive(Serialize, Deserialize)]
pub struct NativeResponse {
    pub client: String,
    pub client_id: u16,
    pub response: Response,
}

#[derive(Serialize, Deserialize)]
pub struct NativeState {
    pub client_id: u16,
    pub snapshot: Snapshot,
    #[serde(default)]
    pub ui_selection: Option<UiSelection>,
    #[serde(default)]
    pub ui_selection_revision: u64,
    #[serde(default)]
    pub ui_selection_epoch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiSelection {
    pub revision: u64,
    pub client: String,
    pub request_id: u64,
    pub terminal_id: String,
}

/// Zellij 0.45.1 reports PaneUpdate then TabUpdate from the same screen state.
#[derive(Default)]
struct Topology {
    pending: Option<PaneManifest>,
    ready: Option<(Vec<TabInfo>, PaneManifest)>,
    // CLI reads republish the same pair; retaining it prevents a second reconcile timer.
    reconciled: Option<(Vec<TabInfo>, PaneManifest)>,
}

impl Topology {
    fn panes(&mut self, manifest: PaneManifest) {
        self.pending = Some(manifest);
        self.ready = None;
    }

    fn tabs(&mut self, tabs: Vec<TabInfo>) -> bool {
        let Some(manifest) = self.pending.take() else {
            return false;
        };
        let positions: BTreeSet<_> = tabs.iter().map(|tab| tab.position).collect();
        if positions.len() != tabs.len() || positions != manifest.panes.keys().copied().collect() {
            return false;
        }
        if self
            .reconciled
            .as_ref()
            .is_some_and(|(previous_tabs, previous_panes)| {
                previous_tabs == &tabs && previous_panes == &manifest
            })
        {
            return false;
        }
        self.ready = Some((tabs, manifest));
        true
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProjectTicket {
    generation: u64,
    owner: u32,
}

#[derive(Deserialize)]
struct ProjectUpdate {
    cwd: String,
    ticket: ProjectTicket,
    project: Option<Project>,
    error: Option<String>,
}

const AGENT_RESULT_MILLIS: u64 = 5_000;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentTicket {
    generation: u64,
    owner: u32,
    client: u16,
    started: u64,
    roots: BTreeMap<u32, i32>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct NativeProgram {
    records: Vec<ProgramStatus>,
}

impl NativeProgram {
    fn dominant(&self) -> Option<&ProgramStatus> {
        self.records
            .iter()
            .max_by_key(|record| record.status().priority())
    }

    fn attention(&self) -> Option<Attention> {
        Attention::aggregate(
            self.records
                .iter()
                .filter(|record| record.status() == Status::Blocked)
                .map(ProgramStatus::attention),
        )
    }

    fn title(&self) -> Option<&str> {
        let record = self.dominant()?;
        self.records
            .iter()
            .find(|r| r.id.is_empty())
            .and_then(|r| r.title.as_deref())
            .or(record.title.as_deref())
            .filter(|title| !title.is_empty())
    }

    fn label(&self) -> Option<String> {
        self.title().map(display_text)
    }

    fn same_display(&self, other: &Self, renamed: bool) -> bool {
        let (Some(previous), Some(current)) = (self.dominant(), other.dominant()) else {
            return false;
        };
        previous.state == current.state
            && self.attention() == other.attention()
            && previous.kind == current.kind
            && previous.progress == current.progress
            && previous.app == current.app
            && previous.msg == current.msg
            && (renamed || self.title() == other.title())
    }

    fn description(&self) -> String {
        let Some(record) = self.dominant() else {
            return String::new();
        };
        let mut description = record
            .msg
            .as_deref()
            .filter(|msg| !msg.is_empty())
            .map(display_text)
            .unwrap_or_default();
        if let Some(progress) = record.progress {
            if !description.is_empty() {
                description.push_str(" · ");
            }
            description.push_str(&format!("{progress}%"));
        }
        description
    }
}

fn display_text(text: &str) -> String {
    crate::ui::clean(text).into_owned()
}

fn native_title<'a>(title: &'a str, harness: Option<&str>) -> &'a str {
    if harness != Some("omp") {
        return title;
    }
    if let Some(label) = title.strip_prefix("π: ") {
        return label;
    }
    let Some(rest) = title.strip_prefix("π ") else {
        return title;
    };
    let Some(separator) = rest.chars().next() else {
        return title;
    };
    if matches!(
        separator,
        '>' | '!' | ':' | '-' | '\\' | '|' | '/' | '○' | '◔' | '◑' | '◕' | '●'
    ) || ('\u{2800}'..='\u{28ff}').contains(&separator)
    {
        return rest[separator.len_utf8()..]
            .strip_prefix(' ')
            .unwrap_or(title);
    }
    title
}

fn clock_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub fn configuration_valid(configuration: &BTreeMap<String, String>, role: &str) -> bool {
    configuration.len() == 7
        && configuration.get("role").map(String::as_str) == Some(role)
        && configuration.get("mode").map(String::as_str) == Some("native")
        && configuration.get("session").is_some_and(|s| !s.is_empty())
        && ["program", "initial_cwd"].iter().all(|key| {
            configuration
                .get(*key)
                .is_some_and(|p| Path::new(p).is_absolute())
        })
        && configuration
            .get("plugin_url")
            .is_some_and(|s| s.starts_with("file:"))
        && configuration
            .get("sidebar_width")
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|w| w > 0)
}

pub fn send(
    owner_id: Option<u32>,
    client: &str,
    client_id: u16,
    request: &Request,
) -> Result<(), serde_json::Error> {
    let payload = serde_json::to_string(&NativeRequest {
        client: client.to_owned(),
        client_id,
        request: request.clone(),
    })?;
    let mut message = MessageToPlugin::new(REQUEST).with_payload(payload);
    if let Some(owner_id) = owner_id {
        message = message.with_destination_plugin_id(owner_id);
    }
    pipe_message_to_plugin(message);
    Ok(())
}

pub fn refresh_ci(
    owner_id: Option<u32>,
    client_id: u16,
    target: &CiRefreshTarget,
) -> Result<(), serde_json::Error> {
    let payload = serde_json::to_string(&NativeCiRefresh {
        client_id,
        target: target.clone(),
    })?;
    let mut message = MessageToPlugin::new(REFRESH_CI).with_payload(payload);
    if let Some(owner_id) = owner_id {
        message = message.with_destination_plugin_id(owner_id);
    }
    pipe_message_to_plugin(message);
    Ok(())
}

pub fn restart_owner(
    configuration: &BTreeMap<String, String>,
    closed_id: u32,
) -> std::io::Result<()> {
    let mut owner_configuration = configuration.clone();
    owner_configuration.insert("role".to_owned(), "native-owner".to_owned());
    owner_configuration.insert(
        "caller_cwd".to_owned(),
        configuration["initial_cwd"].clone(),
    );
    let identity = serde_json::to_vec(&owner_configuration)
        .expect("serializing native owner restart identity");
    let hash = identity.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    let claim = format!(
        "/cache/shephrd-owner-restart-{}-{hash:016x}-{closed_id}",
        get_plugin_ids().zellij_pid,
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(claim)
    {
        Ok(_) => load_new_plugin(
            &configuration["plugin_url"],
            owner_configuration,
            true,
            false,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct NativeWorkspace {
    name: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct NativePane {
    workspace: String,
    tab: usize,
    name: String,
    renamed: bool,
    #[serde(default)]
    has_native_title: bool,
    cwd: Option<String>,
    command: Vec<String>,
    shell: bool,
    // An empty successful command result is metadata too, not a reason to repeat the query.
    #[serde(default)]
    metadata_initialized: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    request_id: u64,
    error: Option<String>,
    focus: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Model {
    configuration: BTreeMap<String, String>,
    instance: String,
    revision: u64,
    // Artifact-shared publication state deduplicates broadcasts across per-client WASM replicas.
    #[serde(default)]
    published_revision: Option<u64>,
    #[serde(default)]
    published_error: Option<String>,
    next_workspace: u64,
    workspaces: BTreeMap<String, NativeWorkspace>,
    tabs: BTreeMap<usize, String>,
    panes: BTreeMap<u32, NativePane>,
    #[serde(default)]
    agents: BTreeMap<u32, AgentExecution>,
    #[serde(default)]
    program_status: BTreeMap<u32, NativeProgram>,
    #[serde(default)]
    agent_roots: BTreeMap<u32, i32>,
    #[serde(default)]
    agent_generation: u64,
    #[serde(default)]
    pending_agents: Option<AgentTicket>,
    #[serde(default)]
    agents_dirty: bool,
    #[serde(default)]
    agent_error: Option<String>,
    #[serde(default)]
    focus: BTreeMap<u16, u32>,
    #[serde(default)]
    workspace_focus: BTreeMap<u16, BTreeMap<String, u32>>,
    #[serde(default)]
    pending_tabs: BTreeSet<usize>,
    #[serde(default)]
    closed_panes: BTreeSet<u32>,
    #[serde(default)]
    closed_tabs: BTreeSet<usize>,
    #[serde(default)]
    frontend_clients: BTreeMap<u32, BTreeSet<u16>>,
    #[serde(default)]
    project_generations: BTreeMap<String, u64>,
    #[serde(default)]
    pending_projects: BTreeMap<String, ProjectTicket>,
    #[serde(default)]
    project_watch_sequence: u64,
    #[serde(default)]
    project_watches: BTreeMap<String, ProjectTicket>,
    // Invalidating a cwd clears failures; unrelated inspector callbacks must not retry discovery.
    #[serde(default)]
    failed_projects: BTreeSet<String>,
    #[serde(default)]
    queries: BTreeSet<String>,
    projects: BTreeMap<String, Project>,
    project_error: Option<String>,
    frontends: BTreeSet<u32>,
    receipts: BTreeMap<String, Receipt>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Project {
    name: String,
    repository: bool,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    branch_changes: Option<shephrd::BranchChanges>,
    #[serde(default)]
    repository_id: Option<String>,
    #[serde(default)]
    ci: Option<CiSnapshot>,
}

impl Project {
    fn identity(&self, cwd: &str) -> ProjectIdentity {
        ProjectIdentity {
            id: self
                .repository_id
                .clone()
                .unwrap_or_else(|| format!("directory:{cwd}")),
            name: self.name.clone(),
            repository: self.repository && self.repository_id.is_some(),
        }
    }
}

impl Model {
    fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        match serde_json::from_slice(bytes) {
            Ok(model) => Ok(model),
            Err(original_error) => {
                // One-time persisted-cache migration, not a legacy public wire format.
                // Only obsolete CI payloads are invalidated; user labels and Git metadata survive.
                let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
                let mut refresh = Vec::new();
                if let Some(projects) = value["projects"].as_object_mut() {
                    for (cwd, project) in projects {
                        if project["ci"]["repository"].is_string() {
                            project["ci"] = serde_json::Value::Null;
                            refresh.push(cwd.clone());
                        }
                    }
                }
                if refresh.is_empty() {
                    return Err(original_error);
                }
                let mut model: Self = serde_json::from_value(value)?;
                for cwd in refresh {
                    let generation = model.project_generations.entry(cwd.clone()).or_default();
                    *generation += 1;
                    model.project_watches.remove(&cwd);
                    model.pending_projects.remove(&cwd);
                }
                model.revision += 1;
                Ok(model)
            }
        }
    }

    fn new(configuration: BTreeMap<String, String>, instance: String) -> Self {
        Self {
            configuration,
            instance,
            revision: 0,
            published_revision: None,
            published_error: None,
            next_workspace: 0,
            workspaces: BTreeMap::new(),
            tabs: BTreeMap::new(),
            panes: BTreeMap::new(),
            agents: BTreeMap::new(),
            program_status: BTreeMap::new(),
            agent_roots: BTreeMap::new(),
            agent_generation: 0,
            pending_agents: None,
            agents_dirty: true,
            agent_error: None,
            focus: BTreeMap::new(),
            workspace_focus: BTreeMap::new(),
            pending_tabs: BTreeSet::new(),
            closed_panes: BTreeSet::new(),
            closed_tabs: BTreeSet::new(),
            frontend_clients: BTreeMap::new(),
            project_generations: BTreeMap::new(),
            pending_projects: BTreeMap::new(),
            project_watch_sequence: 0,
            project_watches: BTreeMap::new(),
            queries: BTreeSet::new(),
            failed_projects: BTreeSet::new(),
            projects: BTreeMap::new(),
            frontends: BTreeSet::new(),
            receipts: BTreeMap::new(),
            project_error: None,
        }
    }

    fn workspace(&mut self, name: String) -> String {
        self.next_workspace += 1;
        let id = format!("native-workspace-{}", self.next_workspace);
        self.workspaces.insert(id.clone(), NativeWorkspace { name });
        id
    }

    fn snapshot(&self, error: Option<String>, client_id: u16) -> Snapshot {
        let panes: Vec<_> = self
            .panes
            .iter()
            .map(|(id, pane)| {
                let agent = self.agents.get(id);
                let program = self.program_status.get(id);
                let project = pane.cwd.as_deref().and_then(|cwd| self.projects.get(cwd));
                let record = program.and_then(NativeProgram::dominant);
                let reported_name = program
                    .and_then(NativeProgram::label)
                    .or_else(|| {
                        pane.has_native_title.then(|| {
                            native_title(
                                &pane.name,
                                record
                                    .and_then(|r| r.app.as_deref())
                                    .or_else(|| agent.map(|agent| agent.harness.as_str())),
                            )
                            .to_owned()
                        })
                    })
                    .or_else(|| {
                        record
                            .and_then(|r| r.app.as_deref())
                            .map(|app| harness_name(app).to_owned())
                    })
                    .or_else(|| agent.map(|agent| harness_name(&agent.harness).to_owned()));
                Pane {
                    id: format!("terminal_{id}"),
                    terminal_id: format!("terminal_{id}"),
                    workspace_id: pane.workspace.clone(),
                    tab_id: format!("tab_{}", pane.tab),
                    name: if pane.renamed {
                        pane.name.clone()
                    } else {
                        reported_name.unwrap_or_else(|| pane.name.clone())
                    },
                    description: if let Some(program) = program {
                        program.description()
                    } else if agent.is_some() {
                        "Native agent · semantic status unavailable".to_owned()
                    } else if pane.command.is_empty() {
                        "Native Zellij terminal · execution unknown".to_owned()
                    } else if pane.shell {
                        format!("Native shell · {}", pane.command.join(" "))
                    } else {
                        format!(
                            "Native command · {} · semantic status unavailable",
                            pane.command.join(" ")
                        )
                    },
                    harness: record
                        .and_then(|r| r.app.clone())
                        .or_else(|| agent.map(|agent| agent.harness.clone())),
                    status: if let Some(record) = record {
                        record.status()
                    } else if pane.shell && agent.is_none() {
                        Status::Idle
                    } else {
                        Status::Unknown
                    },
                    attention: program.and_then(NativeProgram::attention),
                    cwd: pane.cwd.clone(),
                    branch: project.and_then(|project| project.branch.clone()),
                    branch_changes: project.and_then(|project| project.branch_changes.clone()),
                    project: project
                        .filter(|project| project.repository && project.repository_id.is_some())
                        .map(|project| project.identity(pane.cwd.as_deref().unwrap_or_default())),
                    ci: project.and_then(|project| project.ci.clone()),
                }
            })
            .collect();
        let workspaces = self
            .workspaces
            .iter()
            .map(|(id, workspace)| {
                let members = || panes.iter().filter(|p| &p.workspace_id == id);
                let projects: BTreeMap<_, _> = members()
                    .filter_map(|p| p.cwd.as_deref())
                    .filter_map(|cwd| {
                        self.projects.get(cwd).map(|project| {
                            (
                                (
                                    project.repository_id.as_deref().unwrap_or(cwd),
                                    project.repository,
                                ),
                                project.identity(cwd),
                            )
                        })
                    })
                    .collect();
                let active_tab_id = self
                    .workspace_focus
                    .get(&client_id)
                    .and_then(|focus| focus.get(id))
                    .and_then(|pane| self.panes.get(pane))
                    .map(|p| format!("tab_{}", p.tab))
                    .or_else(|| {
                        self.tabs
                            .iter()
                            .find(|(_, w)| *w == id)
                            .map(|(tab, _)| format!("tab_{tab}"))
                    })
                    .unwrap_or_default();
                Workspace {
                    id: id.clone(),
                    name: workspace.name.clone(),
                    active_tab_id,
                    projects: projects.into_values().collect(),
                    status: members()
                        .map(|pane| pane.status)
                        .max_by_key(|status| status.priority())
                        .unwrap_or(Status::Unknown),
                    attention: Attention::aggregate(
                        members()
                            .filter(|pane| pane.status == Status::Blocked)
                            .map(|pane| pane.attention),
                    ),
                }
            })
            .collect();
        Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id: self.instance.clone(),
            revision: self.revision,
            session: self.configuration["session"].clone(),
            machine: None,
            connected: error.is_none(),
            error: error
                .or_else(|| self.project_error.clone())
                .or_else(|| self.agent_error.clone()),
            focused_pane_id: self
                .focus
                .get(&client_id)
                .map(|id| format!("terminal_{id}")),
            workspaces,
            panes,
        }
    }

    // Stable tab IDs, not tab positions, own workspace membership. Existing destinations win;
    // a newly broken-out tab inherits the lowest-ID existing terminal's workspace. An entirely
    // new stock tab adopts a fresh workspace. Splits consequently inherit their native tab.
    fn reconcile(&mut self, tabs: &[TabInfo], manifest: &PaneManifest) -> bool {
        let mut changed = false;
        let mut agent_topology_changed = false;
        // Zellij terminal IDs are monotonic for the server lifetime. Keep close tombstones
        // across topology callbacks so a delayed notification cannot resurrect a terminal.
        self.closed_tabs
            .retain(|id| tabs.iter().any(|tab| tab.tab_id == *id));
        let mut alive = BTreeSet::new();
        let mut ordered: Vec<_> = tabs.iter().collect();
        ordered.sort_by_key(|tab| tab.tab_id);
        for tab in ordered {
            if self.closed_tabs.contains(&tab.tab_id) {
                continue;
            }
            let Some(panes) = manifest.panes.get(&tab.position) else {
                // PaneUpdate and TabUpdate are separate events: an absent tab manifest is
                // not proof that its terminals closed.
                alive.extend(
                    self.panes
                        .iter()
                        .filter(|(_, pane)| pane.tab == tab.tab_id)
                        .map(|(id, _)| *id),
                );
                continue;
            };
            let mut terminals: Vec<_> = panes
                .iter()
                .filter(|p| !p.is_plugin && !self.closed_panes.contains(&p.id))
                .collect();
            terminals.sort_by_key(|p| p.id);
            let workspace = if let Some(workspace) = self.tabs.get(&tab.tab_id) {
                workspace.clone()
            } else {
                changed = true;
                let inherited = terminals
                    .iter()
                    .find_map(|p| self.panes.get(&p.id).map(|p| p.workspace.clone()));
                let workspace = inherited.unwrap_or_else(|| self.workspace(tab.name.clone()));
                self.tabs.insert(tab.tab_id, workspace.clone());
                workspace
            };
            for pane in terminals {
                alive.insert(pane.id);
                let new = !self.panes.contains_key(&pane.id);
                changed |= new;
                agent_topology_changed |= new;
                let entry = self.panes.entry(pane.id).or_insert_with(|| NativePane {
                    workspace: workspace.clone(),
                    tab: tab.tab_id,
                    name: if pane.title.is_empty() {
                        format!("Terminal {}", pane.id)
                    } else {
                        pane.title.clone()
                    },
                    renamed: false,
                    has_native_title: !pane.title.is_empty(),
                    cwd: None,
                    command: Vec::new(),
                    shell: false,
                    metadata_initialized: false,
                });
                if entry.workspace != workspace {
                    entry.workspace = workspace.clone();
                    changed = true;
                    agent_topology_changed = true;
                }
                if entry.tab != tab.tab_id {
                    entry.tab = tab.tab_id;
                    changed = true;
                    agent_topology_changed = true;
                }
                let has_native_title = !pane.title.is_empty();
                if entry.has_native_title != has_native_title {
                    entry.has_native_title = has_native_title;
                    changed = true;
                }
                if !entry.renamed && !pane.title.is_empty() && entry.name != pane.title {
                    entry.name = pane.title.clone();
                    changed = true;
                }
            }
        }
        // Notifications can precede their first topology. Only terminals we previously knew
        // to be alive can be inferred closed from this reconciliation.
        for id in self.panes.keys().filter(|id| !alive.contains(*id)) {
            self.program_status.remove(id);
            self.closed_panes.insert(*id);
        }
        let previous_count = self.panes.len();
        self.panes.retain(|id, _| alive.contains(id));
        changed |= self.panes.len() != previous_count;
        agent_topology_changed |= self.panes.len() != previous_count;
        self.agents.retain(|id, _| alive.contains(id));
        self.program_status
            .retain(|id, _| !self.closed_panes.contains(id));
        self.agent_roots.retain(|id, _| alive.contains(id));
        // Preserve bindings for newly created tabs until their first manifest arrives.
        let known_tabs: BTreeSet<_> = tabs
            .iter()
            .filter(|tab| !self.closed_tabs.contains(&tab.tab_id))
            .map(|t| t.tab_id)
            .collect();
        let previous_count = self.tabs.len();
        self.pending_tabs.retain(|id| !known_tabs.contains(id));
        self.tabs
            .retain(|id, _| known_tabs.contains(id) || self.pending_tabs.contains(id));
        changed |= self.tabs.len() != previous_count;
        let previous_count = self.workspaces.len();
        self.workspaces
            .retain(|id, _| self.tabs.values().any(|workspace| workspace == id));
        changed |= self.workspaces.len() != previous_count;
        let previous_count = self.focus.len();
        self.focus.retain(|_, pane| self.panes.contains_key(pane));
        changed |= self.focus.len() != previous_count;
        for focus in self.workspace_focus.values_mut() {
            focus.retain(|workspace, pane| {
                self.panes
                    .get(pane)
                    .is_some_and(|p| &p.workspace == workspace)
            });
        }
        if agent_topology_changed {
            self.agent_generation += 1;
            self.agents_dirty = true;
        }
        changed
    }

    fn close_pane(&mut self, id: u32) -> bool {
        self.closed_panes.insert(id);
        self.program_status.remove(&id);
        if self.panes.remove(&id).is_none() {
            return false;
        }
        self.focus.retain(|_, pane| *pane != id);
        for focus in self.workspace_focus.values_mut() {
            focus.retain(|_, pane| *pane != id);
        }
        self.invalidate_agent(id);
        self.agent_roots.remove(&id);
        true
    }

    fn record_focus(&mut self, client: u16, pane: u32) -> bool {
        let Some(terminal) = self.panes.get(&pane) else {
            return false;
        };
        let changed = self.focus.insert(client, pane) != Some(pane);
        let previous = self
            .workspace_focus
            .entry(client)
            .or_default()
            .insert(terminal.workspace.clone(), pane);
        changed || previous != Some(pane)
    }

    fn invalidate_project(&mut self, cwd: &str) {
        self.projects.remove(cwd);
        self.failed_projects.remove(cwd);
        *self.project_generations.entry(cwd.to_owned()).or_default() += 1;
    }

    fn observe_cwd(&mut self, id: u32, cwd: String) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        if pane.cwd.as_ref() == Some(&cwd) {
            return false;
        }
        // Directory metadata is shared by every terminal at this cwd, not by executions.
        pane.cwd = Some(cwd);
        true
    }

    fn sync_project_watches(&mut self, owner: u32) -> bool {
        let directories: BTreeSet<_> = self
            .panes
            .values()
            .filter_map(|pane| pane.cwd.clone())
            .collect();
        let removed: Vec<_> = self
            .project_watches
            .keys()
            .filter(|cwd| !directories.contains(*cwd))
            .cloned()
            .collect();
        for cwd in removed {
            // Stop accepting results once the last consumer leaves this directory.
            self.invalidate_project(&cwd);
        }
        self.projects.retain(|cwd, _| directories.contains(cwd));
        self.failed_projects.retain(|cwd| directories.contains(cwd));
        let watches: BTreeMap<_, _> = directories
            .into_iter()
            .map(|cwd| {
                let ticket = self
                    .project_ticket(&cwd, owner)
                    .unwrap_or_else(|| self.pending_projects[&cwd].clone());
                (cwd, ticket)
            })
            .collect();
        if self.project_watches == watches {
            return false;
        }
        self.project_watch_sequence += 1;
        self.project_watches = watches.clone();
        self.pending_projects = watches;
        true
    }

    fn ci_refresh_current(&self, source: u32, client: u16, target: &CiRefreshTarget) -> bool {
        self.frontends.contains(&source)
            && self
                .frontend_clients
                .get(&source)
                .is_some_and(|clients| clients.contains(&client))
            && self.projects.iter().any(|(cwd, project)| {
                self.project_watches
                    .get(cwd)
                    .is_some_and(|ticket| self.project_result_current(cwd, ticket))
                    && project.ci.as_ref().is_some_and(|ci| {
                        ci.repository == target.repository && ci.head_sha == target.head_sha
                    })
            })
    }

    fn project_ticket(&mut self, cwd: &str, owner: u32) -> Option<ProjectTicket> {
        let ticket = ProjectTicket {
            generation: *self.project_generations.get(cwd).unwrap_or(&0),
            owner,
        };
        if self.pending_projects.get(cwd) == Some(&ticket) {
            return None;
        }
        self.pending_projects.insert(cwd.to_owned(), ticket.clone());
        Some(ticket)
    }

    fn project_result_current(&self, cwd: &str, ticket: &ProjectTicket) -> bool {
        self.pending_projects.get(cwd) == Some(ticket)
            && *self.project_generations.get(cwd).unwrap_or(&0) == ticket.generation
    }

    fn apply_project(&mut self, update: ProjectUpdate) -> bool {
        if !self.project_result_current(&update.cwd, &update.ticket) {
            return false;
        }
        if let Some(project) = update.project {
            let changed =
                self.projects.get(&update.cwd) != Some(&project) || self.project_error.is_some();
            self.projects.insert(update.cwd.clone(), project);
            self.failed_projects.remove(&update.cwd);
            self.project_error = None;
            changed
        } else {
            let error = format!(
                "Project discovery failed: {}",
                update.error.as_deref().unwrap_or("missing metadata")
            );
            let changed = self.projects.remove(&update.cwd).is_some()
                || self.project_error.as_ref() != Some(&error);
            self.failed_projects.insert(update.cwd);
            self.project_error = Some(error);
            changed
        }
    }

    fn observe_roots(&mut self, roots: BTreeMap<u32, i32>) -> bool {
        if self.agent_roots == roots {
            return false;
        }
        let previous_count = self.agents.len();
        self.agents
            .retain(|id, agent| roots.get(id) == Some(&agent.root_pid));
        self.agent_roots = roots;
        self.agent_generation += 1;
        self.agents_dirty = true;
        self.agents.len() != previous_count
    }

    fn invalidate_agent(&mut self, id: u32) {
        self.agent_generation += 1;
        self.agents_dirty = true;
        self.agents.remove(&id);
    }

    fn observe_command(&mut self, id: u32, command: Vec<String>) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        if pane.command == command {
            return false;
        }
        pane.shell = is_shell(&command);
        pane.command = command;
        self.invalidate_agent(id);
        true
    }

    fn agent_ticket(&mut self, owner: u32, client: u16, now: u64) -> Option<AgentTicket> {
        if self.panes.is_empty()
            || self
                .pending_agents
                .as_ref()
                .is_some_and(|ticket| now.saturating_sub(ticket.started) < AGENT_RESULT_MILLIS)
        {
            return None;
        }
        let ticket = AgentTicket {
            generation: self.agent_generation,
            owner,
            client,
            started: now,
            roots: self.agent_roots.clone(),
        };
        self.pending_agents = Some(ticket.clone());
        Some(ticket)
    }

    fn agent_result_current(&self, ticket: &AgentTicket, now: u64) -> bool {
        self.pending_agents.as_ref() == Some(ticket)
            && self.agent_generation == ticket.generation
            && self.agent_roots == ticket.roots
            && now.saturating_sub(ticket.started) < AGENT_RESULT_MILLIS
    }

    fn apply_program_status(&mut self, update: ProgramStatusUpdate) -> Result<bool, &'static str> {
        let ProgramStatusUpdate { pane_id, records } = update;
        let mut ids = BTreeSet::new();
        let segment = |value: &str| {
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        };
        let text = |value: &Option<String>, limit: usize| {
            value
                .as_ref()
                .is_none_or(|value| value.len() <= limit && !value.chars().any(char::is_control))
        };
        if records.len() > 256
            || records.iter().any(|record| {
                !ids.insert(record.id.as_str())
                    || record.status() == Status::Unknown
                    || (!record.id.is_empty()
                        && (record.id.len() > 128
                            || record.id.split('/').count() > 8
                            || !record.id.split('/').all(segment)))
                    || !record.app.as_deref().is_none_or(segment)
                    || !text(&record.title, 192)
                    || !text(&record.msg, 2048)
                    || record.kind.as_deref().is_some_and(|kind| {
                        record.state != "blocked"
                            || !matches!(kind, "permission" | "question" | "auth")
                    })
                    || record.progress.is_some_and(|progress| {
                        progress > 100 || !matches!(record.state.as_str(), "working" | "blocked")
                    })
            })
        {
            return Err("Native program update returned invalid records");
        }
        if self.closed_panes.contains(&pane_id) {
            return Ok(false);
        }
        if records.is_empty() {
            return Ok(
                self.program_status.remove(&pane_id).is_some() && self.panes.contains_key(&pane_id)
            );
        }
        let previous = self.program_status.get(&pane_id);
        if previous.is_some_and(|previous| previous.records == records) {
            return Ok(false);
        }
        let program = NativeProgram { records };
        let changed = self.panes.get(&pane_id).is_some_and(|pane| {
            previous.is_none_or(|previous| !previous.same_display(&program, pane.renamed))
        });
        self.program_status.insert(pane_id, program);
        Ok(changed)
    }

    fn apply_agents(
        &mut self,
        ticket: &AgentTicket,
        response: InspectResponse,
        now: u64,
    ) -> Result<bool, &'static str> {
        if !self.agent_result_current(ticket, now) {
            return Err("Stale native agent inspection");
        }
        let mut agents = BTreeMap::new();
        for execution in response.executions {
            if ticket.roots.get(&execution.pane_id) != Some(&execution.root_pid)
                || execution.pid <= 0
                || execution.identity.is_empty()
                || execution.harness.is_empty()
                || !self.panes.contains_key(&execution.pane_id)
                || agents.contains_key(&execution.pane_id)
            {
                return Err("Native agent inspection returned conflicting execution identities");
            }
            agents.insert(execution.pane_id, execution);
        }
        // Process discovery supplies identity/CWD only; semantic state remains terminal-owned.
        let mut changed = self.agent_error.is_some();
        for id in self.panes.keys() {
            if self
                .program_status
                .get(id)
                .and_then(NativeProgram::dominant)
                .is_none_or(|record| record.app.is_none())
            {
                changed |= self.agents.get(id).map(|agent| &agent.harness)
                    != agents.get(id).map(|agent| &agent.harness);
            }
        }
        for (id, agent) in &agents {
            if let Some(cwd) = &agent.cwd {
                changed |= self.observe_cwd(*id, cwd.clone());
            }
        }
        self.agents = agents;
        self.pending_agents = None;
        self.agent_error = None;
        Ok(changed)
    }
}

#[derive(Default)]
pub struct Owner {
    configuration: BTreeMap<String, String>,
    granted: bool,
    permission_requested: bool,
    model: Option<Model>,
    storage: PathBuf,
    storage_bytes: Vec<u8>,
    storage_buffer: Vec<u8>,
    topology: Topology,
    client_id: u16,
    id: u32,
    server: i32,
    reconcile_pending: bool,
    refresh_metadata: bool,
    error: Option<String>,
    queued: Vec<PipeMessage>,
    // UI intent is ephemeral and client-isolated; canonical focus remains host-authoritative.
    ui_selections: BTreeMap<u16, UiSelection>,
    ui_selection_revisions: BTreeMap<u16, u64>,
    selection_revision: u64,
    published_selection_revision: u64,
    selection_epoch: String,
}

impl Owner {
    pub fn load(&mut self, configuration: &BTreeMap<String, String>) {
        let mut configuration = configuration.clone();
        // This key belongs to SDK routing, not the seven-key native application contract.
        configuration.remove("caller_cwd");
        let ids = get_plugin_ids();
        self.client_id = ids.client_id;
        self.id = ids.plugin_id;
        self.server = ids.zellij_pid as i32;
        self.selection_epoch = format!(
            "{}:{}:{}",
            self.id,
            self.client_id,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        self.ui_selections.clear();
        self.ui_selection_revisions.clear();
        self.selection_revision = 0;
        self.published_selection_revision = 0;
        self.configuration = configuration.clone();
        self.refresh_metadata = true;
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::CwdChanged,
            EventType::CommandChanged,
            EventType::CommandPaneExited,
            EventType::PaneClosed,
            EventType::RunCommandResult,
            EventType::Timer,
        ]);
        if !configuration_valid(&configuration, "native-owner") {
            self.error = Some(format!(
                "Invalid native owner configuration: keys {:?}",
                configuration.keys().collect::<Vec<_>>()
            ));
            return;
        }
        let pid = ids.zellij_pid;
        let identity = serde_json::to_vec(&configuration).expect("serializing owner configuration");
        let hash = identity.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        self.storage = PathBuf::from(format!("/cache/shephrd-native-{pid}-{hash:016x}.json"));
        match std::fs::read(&self.storage) {
            Ok(bytes) => match Model::decode(&bytes) {
                Ok(model) if model.configuration == configuration => {
                    self.model = Some(model);
                    self.storage_bytes = bytes;
                }
                _ => self.error = Some("Native metadata cache is corrupt or has a conflicting identity; refusing to reset labels".to_owned()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.model = Some(Model::new(configuration.clone(), format!("native:{pid}:{hash:016x}")));
            }
            Err(error) => self.error = Some(format!("Cannot recover native metadata: {error}")),
        }
    }

    // Zellij serializes callbacks for a plugin ID, including its per-client WASM replicas.
    // /cache is artifact-shared (unlike per-client /data). Refreshing before every callback
    // makes replicas consumers of one canonical model. Focus is sampled only at commit boundaries.
    fn recover(&mut self) {
        self.recover_model();
    }

    fn recover_model(&mut self) {
        self.storage_buffer.clear();
        let result = std::fs::File::open(&self.storage)
            .and_then(|mut file| file.read_to_end(&mut self.storage_buffer));
        match result {
            Ok(_) if self.model.is_some() && self.storage_buffer == self.storage_bytes => {}
            Ok(_) => match Model::decode(&self.storage_buffer) {
                Ok(model) if model.configuration == self.configuration => {
                    self.model = Some(model);
                    std::mem::swap(&mut self.storage_bytes, &mut self.storage_buffer);
                }
                _ => {
                    self.error = Some(
                        "Cannot read authoritative native metadata; refusing to overwrite it"
                            .to_owned(),
                    )
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                self.error = Some(format!(
                    "Cannot recover authoritative native metadata: {error}"
                ))
            }
        }
    }

    fn save(&mut self) {
        let Some(model) = &self.model else { return };
        if self.error.is_some() {
            return;
        }
        self.storage_buffer.clear();
        let result = serde_json::to_writer(&mut self.storage_buffer, model)
            .map_err(std::io::Error::other)
            .and_then(|()| {
                if self.storage_buffer == self.storage_bytes {
                    return Ok(());
                }
                let temporary = self.storage.with_extension("pending");
                std::fs::write(&temporary, &self.storage_buffer)?;
                std::fs::rename(temporary, &self.storage)
            });
        if let Err(error) = result {
            self.storage_bytes.clear();
            self.error = Some(format!("Cannot retain native metadata: {error}"));
        } else {
            std::mem::swap(&mut self.storage_bytes, &mut self.storage_buffer);
        }
    }

    fn synchronize_focus(&mut self, host_update: bool) -> bool {
        let Ok((_, PaneId::Terminal(pane))) = get_focused_pane_info() else {
            return false;
        };
        self.apply_host_focus(pane, host_update)
    }

    fn apply_host_focus(&mut self, pane: u32, host_update: bool) -> bool {
        let changed = if let Some(model) = self.model.as_mut() {
            let changed = model.record_focus(self.client_id, pane);
            if changed {
                model.revision += 1;
            }
            changed
        } else {
            false
        };
        let confirmed = host_update
            && self
                .ui_selections
                .get(&self.client_id)
                .is_some_and(|selection| terminal_id(&selection.terminal_id) == Some(pane));
        if confirmed {
            self.clear_selection(self.client_id, None);
        }
        changed || confirmed
    }

    fn select_ui(&mut self, client: &str, request_id: u64, pane: u32) {
        self.selection_revision += 1;
        self.ui_selection_revisions
            .insert(self.client_id, self.selection_revision);
        self.ui_selections.insert(
            self.client_id,
            UiSelection {
                revision: self.selection_revision,
                client: client.to_owned(),
                request_id,
                terminal_id: format!("terminal_{pane}"),
            },
        );
    }

    fn clear_selection(&mut self, client_id: u16, expected: Option<(&str, u64)>) {
        let Some(selection) = self.ui_selections.get(&client_id) else {
            return;
        };
        if expected.is_some_and(|(client, request_id)| {
            selection.client != client || selection.request_id != request_id
        }) {
            return;
        }
        self.ui_selections.remove(&client_id);
        self.selection_revision += 1;
        self.ui_selection_revisions
            .insert(client_id, self.selection_revision);
    }

    fn prune_selections(&mut self) {
        let Some(model) = self.model.as_ref() else {
            return;
        };
        self.ui_selections.retain(|client_id, selection| {
            let alive = terminal_id(&selection.terminal_id)
                .is_some_and(|pane| model.panes.contains_key(&pane));
            if !alive {
                self.selection_revision += 1;
                self.ui_selection_revisions
                    .insert(*client_id, self.selection_revision);
            }
            alive
        });
    }

    fn state(&self, client_id: u16) -> Option<NativeState> {
        let own_client = client_id == self.client_id;
        Some(NativeState {
            client_id,
            snapshot: self.model.as_ref()?.snapshot(self.error.clone(), client_id),
            ui_selection: if own_client {
                self.ui_selections.get(&client_id).cloned()
            } else {
                None
            },
            ui_selection_revision: if own_client {
                self.ui_selection_revisions
                    .get(&client_id)
                    .copied()
                    .unwrap_or_default()
            } else {
                0
            },
            // Other owner-client replicas publish canonical state, not this client's relay epoch.
            ui_selection_epoch: if own_client {
                self.selection_epoch.clone()
            } else {
                String::new()
            },
        })
    }

    fn publish(&mut self) {
        self.prune_selections();
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let changed = model.published_revision != Some(model.revision)
            || model.published_error != self.error
            || self.published_selection_revision != self.selection_revision;
        if changed {
            model.published_revision = Some(model.revision);
            model.published_error = self.error.clone();
            self.published_selection_revision = self.selection_revision;
        }
        self.save();
        if !changed {
            return;
        }
        let Some(model) = &self.model else { return };
        for (frontend, clients) in &model.frontend_clients {
            for client_id in clients {
                let state = self.state(*client_id).expect("native model available");
                let payload = serde_json::to_string(&state).expect("serializing native state");
                pipe_message_to_plugin(
                    MessageToPlugin::new(STATE)
                        .with_destination_plugin_id(*frontend)
                        .with_payload(payload),
                );
            }
        }
    }

    fn schedule(&mut self) {
        if !self.reconcile_pending {
            self.reconcile_pending = true;
            set_timeout(0.03);
        }
    }

    fn reconcile(&mut self) {
        if self.error.is_some() {
            return;
        }
        let Some((tabs, manifest)) = self.topology.ready.take() else {
            return;
        };
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let pending: Vec<_> = model.pending_tabs.iter().copied().collect();
        for id in pending {
            if get_tab_info(id).is_none() {
                model.pending_tabs.remove(&id);
            }
        }
        let generation = model.agent_generation;
        let mut changed = model.reconcile(&tabs, &manifest);
        let refresh_metadata = std::mem::take(&mut self.refresh_metadata);
        let metadata: Vec<_> = model
            .panes
            .iter()
            .filter(|(_, pane)| refresh_metadata || !pane.metadata_initialized)
            .map(|(id, _)| *id)
            .collect();
        for id in metadata {
            let cwd = get_pane_cwd(PaneId::Terminal(id));
            let command = get_pane_running_command(PaneId::Terminal(id));
            let initialized = cwd.is_ok() && command.is_ok();
            if let Ok(cwd) = cwd {
                let cwd = cwd.to_string_lossy().into_owned();
                changed |= model.observe_cwd(id, cwd);
            }
            if let Ok(command) = command {
                changed |= model.observe_command(id, command);
            }
            model.panes.get_mut(&id).unwrap().metadata_initialized = initialized;
        }
        let agents_changed = generation != model.agent_generation;
        if changed {
            model.revision += 1;
        }
        self.topology.reconciled = Some((tabs, manifest));
        let focus_changed = self.synchronize_focus(true);
        if changed || refresh_metadata {
            self.projects();
        }
        if agents_changed {
            self.request_agents();
        }
        if changed || focus_changed {
            self.publish();
        } else {
            self.save();
        }
    }

    fn projects(&mut self) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        if !model.sync_project_watches(self.id) {
            return;
        }
        self.project_watch_command(&[]);
    }

    fn project_watch_command(&self, refresh: &[CiRefreshTarget]) {
        let Some(model) = self.model.as_ref() else {
            return;
        };
        let json = serde_json::json!({
            "server": self.server,
            "owner": self.id,
            "session": self.configuration["session"],
            "sequence": model.project_watch_sequence,
            "watches": model.project_watches,
            "refresh": refresh,
        })
        .to_string();
        // Each update replaces the helper's full subscription set, including removed cwd's.
        // Filesystem callbacks retain these tickets; generations reject old snapshots.
        run_command(
            &[
                &self.configuration["program"],
                "project-watch",
                "--json",
                &json,
            ],
            BTreeMap::from([(
                "native_project_watch".to_owned(),
                model.project_watch_sequence.to_string(),
            )]),
        );
    }

    fn observe_processes(&mut self) -> bool {
        let Some(model) = self.model.as_mut() else {
            return false;
        };
        let mut roots = BTreeMap::new();
        for id in model.panes.keys() {
            if let Ok(pid) = get_pane_pid(PaneId::Terminal(*id))
                && pid > 0
            {
                roots.insert(*id, pid);
            }
        }
        let changed = model.observe_roots(roots);
        if changed {
            model.revision += 1;
        }
        changed
    }

    fn agents(&mut self) {
        if self.error.is_some() {
            return;
        }
        let now = clock_millis();
        if !self.model.as_ref().is_some_and(|model| model.agents_dirty) {
            return;
        }
        let changed = self.observe_processes();
        let Some(model) = self.model.as_mut() else {
            return;
        };
        if model
            .pending_agents
            .as_ref()
            .is_some_and(|ticket| now.saturating_sub(ticket.started) < AGENT_RESULT_MILLIS)
        {
            if changed {
                self.publish();
            } else {
                self.save();
            }
            return;
        }
        let ticket = model.agent_ticket(self.id, self.client_id, now);
        if ticket.is_some() {
            model.agents_dirty = false;
            set_timeout(AGENT_RESULT_MILLIS as f64 / 1_000.0);
        }
        let Some(ticket) = ticket else {
            if changed {
                self.publish();
            } else {
                self.save();
            }
            return;
        };
        let request = InspectRequest {
            panes: ticket
                .roots
                .iter()
                .map(|(id, root_pid)| PaneProcess {
                    pane_id: *id,
                    root_pid: *root_pid,
                })
                .collect(),
        };
        let request = serde_json::to_string(&request).expect("serializing native agent request");
        let context = serde_json::to_string(&ticket).expect("serializing native agent ticket");
        if request.len() > MAX_MESSAGE_BYTES || context.len() > MAX_MESSAGE_BYTES {
            self.agent_failure("Native agent inspection exceeds the message limit".to_owned());
            return;
        }
        if changed {
            self.publish();
        } else {
            self.save();
        }
        run_command(
            &[
                &self.configuration["program"],
                "agent",
                "inspect",
                "--json",
                &request,
            ],
            BTreeMap::from([("native_agent".to_owned(), context)]),
        );
    }

    fn agent_failure(&mut self, error: String) {
        if let Some(model) = self.model.as_mut() {
            // Failure to inspect is not evidence that the last confirmed execution ended.
            // Native lifecycle events invalidate identity; this deadline only retires its ticket.
            let changed = model.agent_error.as_ref() != Some(&error);
            model.pending_agents = None;
            model.agent_error = Some(error);
            if changed {
                model.revision += 1;
            }
        }
        self.publish();
    }

    fn agent_response(&mut self, code: Option<i32>, stdout: &[u8], stderr: &[u8], context: &str) {
        if context.len() > MAX_MESSAGE_BYTES {
            return;
        }
        let Ok(ticket) = serde_json::from_str::<AgentTicket>(context) else {
            return;
        };
        if ticket.owner != self.id
            || ticket.client != self.client_id
            || !self
                .model
                .as_ref()
                .is_some_and(|model| model.pending_agents.as_ref() == Some(&ticket))
        {
            return;
        }
        // Consume already-delivered topology before checking execution scope.
        if self.topology.ready.is_some() {
            self.reconcile();
        }
        let changed = self.observe_processes();
        let now = clock_millis();
        let Some(model) = self.model.as_mut() else {
            return;
        };
        if self.topology.pending.is_some() || !model.agent_result_current(&ticket, now) {
            model.pending_agents = None;
            if changed {
                self.publish();
            } else {
                self.save();
            }
            self.schedule();
            return;
        }
        if code != Some(0) || stdout.len() > MAX_MESSAGE_BYTES {
            let stderr = &stderr[..stderr.len().min(4_096)];
            self.agent_failure(format!(
                "Native agent inspection failed: {}",
                crate::ui::clean(&String::from_utf8_lossy(stderr))
            ));
            return;
        }
        let Ok(response) = serde_json::from_slice::<InspectResponse>(stdout) else {
            self.agent_failure("Native agent inspection returned malformed JSON".to_owned());
            return;
        };
        match model.apply_agents(&ticket, response, now) {
            Ok(applied) => {
                if applied {
                    model.revision += 1;
                    self.projects();
                }
                if applied || changed {
                    self.publish();
                } else {
                    self.save();
                }
            }
            Err(error) => self.agent_failure(error.to_owned()),
        }
        if self.model.as_ref().is_some_and(|model| model.agents_dirty) {
            self.schedule();
        }
    }

    pub fn event(&mut self, event: Event) -> bool {
        if self.granted {
            self.recover();
        }
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                self.recover();
                self.save();
                subscribe(&[EventType::CustomMessage]);
                if let Some(announcement) = self.state(self.client_id) {
                    let payload = serde_json::to_string(&announcement)
                        .expect("serializing native owner announcement");
                    pipe_message_to_plugin(MessageToPlugin::new(STATE).with_payload(payload));
                }
                for message in std::mem::take(&mut self.queued) {
                    self.pipe(message);
                }
                self.schedule();
                if let Some(model) = self.model.as_mut() {
                    model.agents_dirty = true;
                }
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Native owner permissions denied".to_owned())
            }
            Event::CustomMessage(name, payload) if self.granted && name == "program_status" => {
                if payload.len() > MAX_MESSAGE_BYTES {
                    return false;
                }
                let Ok(update) = serde_json::from_str::<ProgramStatusUpdate>(&payload) else {
                    return false;
                };
                let Some(model) = self.model.as_mut() else {
                    return false;
                };
                match model.apply_program_status(update) {
                    Ok(true) => {
                        model.revision += 1;
                        self.publish();
                    }
                    Ok(false) => self.save(),
                    Err(_) => {}
                }
            }
            Event::PaneUpdate(manifest) if self.granted => {
                self.topology.panes(manifest);
                if self.synchronize_focus(true) {
                    self.publish();
                }
            }
            Event::TabUpdate(tabs) if self.granted => {
                let topology_changed = self.topology.tabs(tabs);
                if self.synchronize_focus(true) {
                    self.publish();
                }
                if topology_changed {
                    self.schedule();
                }
            }
            Event::Timer(_) if self.granted => {
                self.reconcile_pending = false;
                self.reconcile();
                let timed_out = self.model.as_ref().is_some_and(|model| {
                    model.pending_agents.as_ref().is_some_and(|ticket| {
                        clock_millis().saturating_sub(ticket.started) >= AGENT_RESULT_MILLIS
                    })
                });
                if timed_out {
                    self.agent_failure("Native agent inspection timed out".to_owned());
                }
                self.agents();
            }
            Event::CwdChanged(PaneId::Terminal(id), cwd, _) if self.granted => {
                let changed = self.model.as_mut().is_some_and(|model| {
                    if model.observe_cwd(id, cwd.to_string_lossy().into_owned()) {
                        model.revision += 1;
                        true
                    } else {
                        false
                    }
                });
                if changed {
                    self.projects();
                    self.request_agents();
                    self.publish();
                }
            }
            Event::CommandChanged(PaneId::Terminal(id), command, _, _) if self.granted => {
                if let Some(model) = self.model.as_mut()
                    && model.observe_command(id, command)
                {
                    model.revision += 1;
                    self.projects();
                    self.publish();
                }
                self.request_agents();
            }
            Event::CommandPaneExited(id, _, _) if self.granted => {
                if let Some(model) = self.model.as_mut() {
                    let mut changed = model.agents.contains_key(&id);
                    model.invalidate_agent(id);
                    if let Some(program) = model.program_status.get_mut(&id) {
                        let previous = program.records.len();
                        program.records.retain(|record| {
                            !matches!(record.state.as_str(), "working" | "blocked")
                        });
                        changed |= previous != program.records.len();
                    }
                    model
                        .program_status
                        .retain(|_, program| !program.records.is_empty());
                    if changed {
                        model.revision += 1;
                    }
                }
                self.publish();
                self.schedule();
            }
            Event::PaneClosed(PaneId::Terminal(id)) if self.granted => {
                if let Some(model) = self.model.as_mut()
                    && model.close_pane(id)
                {
                    model.revision += 1;
                    self.projects();
                    self.schedule();
                    self.publish();
                } else {
                    self.save();
                }
            }
            Event::PaneClosed(PaneId::Plugin(id)) if self.granted => {
                if let Some(model) = self.model.as_mut() {
                    model.frontends.remove(&id);
                    model.frontend_clients.remove(&id);
                    let prefix = format!("{id}:");
                    model.receipts.retain(|key, _| !key.starts_with(&prefix));
                }
                self.save();
            }
            Event::RunCommandResult(code, stdout, stderr, context) if self.granted => {
                if let Some(context) = context.get("native_agent") {
                    self.agent_response(code, &stdout, &stderr, context);
                    return false;
                }
                if context
                    .get("native_project_watch")
                    .and_then(|sequence| sequence.parse::<u64>().ok())
                    .is_some_and(|sequence| {
                        self.model
                            .as_ref()
                            .is_some_and(|model| model.project_watch_sequence == sequence)
                    })
                    && code != Some(0)
                {
                    let error = format!(
                        "Project watcher failed: {}",
                        crate::ui::clean(&String::from_utf8_lossy(&stderr))
                    );
                    if let Some(model) = self.model.as_mut() {
                        if model.project_error.as_ref() != Some(&error) {
                            model.revision += 1;
                        }
                        model.project_error = Some(error);
                    }
                    self.publish();
                }
            }
            _ => {}
        }
        false
    }

    fn request_agents(&mut self) {
        if let Some(model) = self.model.as_mut() {
            model.agents_dirty = true;
        }
        self.schedule();
    }

    fn action(&mut self, action: Action) -> Result<Option<u32>, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let model = self
            .model
            .as_mut()
            .ok_or("Native owner metadata unavailable")?;
        match action {
            Action::Snapshot | Action::RegisterFrontend => Ok(None),
            Action::SelectPane { pane_id } | Action::FocusPane { pane_id } => {
                let id = terminal_id(&pane_id).ok_or("Invalid native terminal identity")?;
                let pane = model.panes.get(&id).ok_or("Terminal is no longer open")?;
                if !model.workspaces.contains_key(&pane.workspace) {
                    return Err("Workspace is no longer open".to_owned());
                }
                Ok(Some(id))
            }
            Action::SelectWorkspace { workspace_id } => {
                if !model.workspaces.contains_key(&workspace_id) {
                    return Err("Workspace is no longer open".to_owned());
                }
                let id = model
                    .workspace_focus
                    .get(&self.client_id)
                    .and_then(|focus| focus.get(&workspace_id))
                    .copied()
                    .or_else(|| {
                        model
                            .panes
                            .iter()
                            .find(|(_, p)| p.workspace == workspace_id)
                            .map(|(id, _)| *id)
                    });
                if let Some(id) = id {
                    Ok(Some(id))
                } else {
                    self.create_tab(Some(workspace_id), None, None)
                }
            }
            Action::CreateWorkspace { label, cwd } => self.create_tab(None, label, cwd),
            Action::CreatePane { workspace_id, cwd } => {
                self.create_tab(Some(workspace_id), None, cwd)
            }
            Action::RenameWorkspace {
                workspace_id,
                label,
            } => {
                let workspace = model
                    .workspaces
                    .get_mut(&workspace_id)
                    .ok_or("Workspace is no longer open")?;
                workspace.name = label.clone();
                for (tab, workspace) in &model.tabs {
                    if workspace == &workspace_id {
                        rename_tab_with_id(*tab as u64, &label);
                    }
                }
                Ok(None)
            }
            Action::RenamePane { pane_id, label } => {
                let id = terminal_id(&pane_id).ok_or("Invalid native terminal identity")?;
                let pane = model
                    .panes
                    .get_mut(&id)
                    .ok_or("Terminal is no longer open")?;
                pane.name = label.clone();
                pane.renamed = true;
                rename_terminal_pane(id, label);
                Ok(None)
            }
            Action::ClosePane { pane_id } => {
                let id = terminal_id(&pane_id).ok_or("Invalid native terminal identity")?;
                if !model.close_pane(id) {
                    return Err("Terminal is no longer open".to_owned());
                }
                close_terminal_pane(id);
                self.schedule();
                Ok(None)
            }
            Action::CloseWorkspace { workspace_id } => {
                if model.workspaces.remove(&workspace_id).is_none() {
                    return Err("Workspace is no longer open".to_owned());
                }
                let tabs: Vec<_> = model
                    .tabs
                    .iter()
                    .filter(|(_, w)| *w == &workspace_id)
                    .map(|(tab, _)| *tab)
                    .collect();
                model.tabs.retain(|_, workspace| workspace != &workspace_id);
                let panes: Vec<_> = model
                    .panes
                    .iter()
                    .filter(|(_, pane)| pane.workspace == workspace_id)
                    .map(|(id, _)| *id)
                    .collect();
                for id in panes {
                    model.close_pane(id);
                }
                for tab in tabs {
                    model.pending_tabs.remove(&tab);
                    model.closed_tabs.insert(tab);
                    close_tab_with_id(tab as u64);
                }
                self.schedule();
                Ok(None)
            }
            Action::PaneClosed { .. }
            | Action::RegisterPane { .. }
            | Action::ResolveAttachment { .. } => {
                Err("Attachment actions are unavailable for native terminals".to_owned())
            }
        }
    }

    fn create_tab(
        &mut self,
        workspace: Option<String>,
        label: Option<String>,
        cwd: Option<String>,
    ) -> Result<Option<u32>, String> {
        let model = self
            .model
            .as_mut()
            .ok_or("Native owner metadata unavailable")?;
        let cwd = cwd
            .or_else(|| {
                workspace.as_ref().and_then(|w| {
                    model
                        .panes
                        .values()
                        .find(|p| &p.workspace == w)
                        .and_then(|p| p.cwd.clone())
                })
            })
            .unwrap_or_else(|| self.configuration["initial_cwd"].clone());
        if !Path::new(&cwd).is_absolute() {
            return Err("Terminal directory must be absolute".to_owned());
        }
        let name = match &workspace {
            Some(id) => model
                .workspaces
                .get(id)
                .ok_or("Workspace is no longer open")?
                .name
                .clone(),
            None => label.unwrap_or_else(|| format!("Workspace {}", model.next_workspace + 1)),
        };
        let mut sidebar_configuration = self.configuration.clone();
        sidebar_configuration.insert("role".to_owned(), "sidebar".to_owned());
        let width = self.configuration["sidebar_width"]
            .parse()
            .map_err(|_| "Invalid sidebar width")?;
        let layout = shephrd::native_layout(
            &self.configuration["plugin_url"],
            &sidebar_configuration,
            &name,
            &cwd,
            width,
        )
        .map_err(|error| format!("Cannot create native terminal layout: {error}"))?;
        let tabs = new_tabs_with_layout(&layout);
        if tabs.is_empty() {
            return Err("Zellij could not create the native terminal tab".to_owned());
        }
        let workspace = workspace.unwrap_or_else(|| model.workspace(name));
        for id in &tabs {
            model.tabs.insert(*id, workspace.clone());
            model.pending_tabs.insert(*id);
        }
        // Lifecycle events will populate the actual native pane; no fabricated terminal record.
        self.schedule();
        Ok(None)
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == REFRESH_CI {
            let PipeSource::Plugin(source) = message.source else {
                return false;
            };
            if !self.granted {
                return false;
            }
            let Some(payload) = message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= MAX_MESSAGE_BYTES)
            else {
                return false;
            };
            let Ok(refresh) = serde_json::from_str::<NativeCiRefresh>(payload) else {
                return false;
            };
            if refresh.client_id != self.client_id {
                return false;
            }
            self.recover();
            if self.model.as_ref().is_some_and(|model| {
                model.ci_refresh_current(source, self.client_id, &refresh.target)
            }) {
                // Same subscriptions/tickets: no Git rediscovery or cache invalidation.
                // The normal command-result path reports helper failures.
                self.project_watch_command(&[refresh.target]);
            }
            return false;
        }
        if message.name == PROJECT {
            let PipeSource::Cli(source) = &message.source else {
                return false;
            };
            // Every replica acknowledges CLI input, including unauthorized/stale replicas.
            unblock_cli_pipe_input(source);
            if !self.granted {
                return false;
            }
            let Some(payload) = message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= MAX_MESSAGE_BYTES)
            else {
                return false;
            };
            let Ok(update) = serde_json::from_str::<ProjectUpdate>(payload) else {
                return false;
            };
            self.recover();
            let Some(model) = self.model.as_mut() else {
                return false;
            };
            if update.ticket.owner != self.id {
                return false;
            }
            let changed = model.apply_project(update);
            if changed {
                model.revision += 1;
                self.publish();
            }
            return false;
        }
        if message.name == QUERY {
            let PipeSource::Cli(source) = &message.source else {
                return false;
            };
            if !self.granted {
                self.queued.push(message);
                return false;
            }
            self.recover();
            let valid = message.payload.as_deref().is_none_or(|payload| {
                serde_json::from_str::<Action>(payload).is_ok_and(|a| a == Action::Snapshot)
                    || serde_json::from_str::<Request>(payload).is_ok_and(|r| {
                        r.protocol == PROTOCOL_VERSION && r.action == Action::Snapshot
                    })
            });
            if let Some(model) = self.model.as_mut()
                && !model.queries.insert(source.clone())
            {
                unblock_cli_pipe_input(source);
                return false;
            }
            if self.synchronize_focus(false) {
                self.publish();
            } else {
                self.save();
            }
            let output = if valid {
                self.model
                    .as_ref()
                    .map(|m| {
                        serde_json::to_string(&m.snapshot(self.error.clone(), self.client_id))
                            .expect("serializing queried native state")
                    })
                    .unwrap_or_else(|| serde_json::json!({"error": self.error}).to_string())
            } else {
                serde_json::json!({"error": "Read-only query accepts no payload or a snapshot action/request"}).to_string()
            };
            cli_pipe_output(source, &format!("{output}\n"));
            unblock_cli_pipe_input(source);
            return false;
        }
        if message.name != REQUEST {
            return false;
        }
        let PipeSource::Plugin(source) = &message.source else {
            return false;
        };
        let source = *source;
        let Some(payload) = message
            .payload
            .as_deref()
            .filter(|p| p.len() <= MAX_MESSAGE_BYTES)
        else {
            return false;
        };
        let Ok(envelope) = serde_json::from_str::<NativeRequest>(payload) else {
            return false;
        };
        if envelope.request.protocol != PROTOCOL_VERSION || envelope.client_id != self.client_id {
            return false;
        }
        // A background owner starts before terminal panes exist. Wait for an authorized
        // frontend to register, when its artifact permission cache can grant this service.
        if !self.granted {
            if self.queued.len() < 128 {
                self.queued.push(message);
            }
            if !self.permission_requested {
                self.permission_requested = true;
                request_permission(super::PERMISSIONS);
            }
            return false;
        }
        self.recover();
        let focus_changed = self.synchronize_focus(false);
        let key = format!("{source}:{}", envelope.client);
        let receipt = self
            .model
            .as_ref()
            .and_then(|m| m.receipts.get(&key))
            .filter(|r| r.request_id >= envelope.request.request_id)
            .cloned();
        let send_current_state =
            receipt.is_some() || matches!(envelope.request.action, Action::RegisterFrontend);
        if receipt.is_some() && focus_changed {
            self.publish();
        }
        let receipt = receipt.unwrap_or_else(|| {
            if let Some(model) = self.model.as_mut() {
                model.frontends.insert(source);
                model
                    .frontend_clients
                    .entry(source)
                    .or_default()
                    .insert(self.client_id);
            }
            let changes_state = !matches!(
                envelope.request.action,
                Action::Snapshot | Action::RegisterFrontend
            );
            let selects_ui = matches!(
                envelope.request.action,
                Action::SelectPane { .. } | Action::SelectWorkspace { .. }
            );
            let result = self.action(envelope.request.action);
            let receipt = Receipt {
                request_id: envelope.request.request_id,
                error: result.as_ref().err().cloned(),
                focus: result.ok().flatten(),
            };
            if selects_ui {
                if receipt.error.is_none() {
                    if let Some(pane) = receipt.focus {
                        self.select_ui(&envelope.client, receipt.request_id, pane);
                    }
                } else {
                    self.clear_selection(
                        self.client_id,
                        Some((&envelope.client, receipt.request_id)),
                    );
                }
            }
            if let Some(model) = self.model.as_mut() {
                if changes_state && receipt.error.is_none() {
                    model.revision += 1;
                }
                model.receipts.insert(key, receipt.clone());
            }
            self.publish();
            receipt
        });
        if send_current_state && let Some(state) = self.state(self.client_id) {
            let payload =
                serde_json::to_string(&state).expect("serializing native selection state");
            pipe_message_to_plugin(
                MessageToPlugin::new(STATE)
                    .with_destination_plugin_id(source)
                    .with_payload(payload),
            );
        }
        // Registration and duplicate requests always receive their full canonical state here,
        // even when the shared publication revision already matches.
        let response = NativeResponse {
            client: envelope.client,
            client_id: self.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: envelope.request.request_id,
                state: self
                    .model
                    .as_ref()
                    .map(|m| m.snapshot(self.error.clone(), self.client_id)),
                effects: receipt
                    .focus
                    .map(|zellij_pane_id| Effect::FocusPane { zellij_pane_id })
                    .into_iter()
                    .collect(),
                error: receipt.error.or_else(|| self.error.clone()),
            },
        };
        let payload = serde_json::to_string(&response).expect("serializing native response");
        pipe_message_to_plugin(
            MessageToPlugin::new(RESPONSE)
                .with_destination_plugin_id(source)
                .with_payload(payload),
        );
        false
    }
}

fn terminal_id(id: &str) -> Option<u32> {
    id.strip_prefix("terminal_")?.parse().ok()
}

fn is_shell(command: &[String]) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    let name = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim_start_matches('-');
    matches!(
        name,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "nu" | "elvish" | "xonsh"
    ) && command
        .iter()
        .skip(1)
        .all(|arg| matches!(arg.as_str(), "-l" | "-i" | "--login" | "--interactive"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model::new(
            BTreeMap::from([("session".to_owned(), "test".to_owned())]),
            "test-instance".to_owned(),
        )
    }
    fn tab(id: usize, position: usize) -> TabInfo {
        TabInfo {
            tab_id: id,
            position,
            name: format!("Tab {id}"),
            ..TabInfo::default()
        }
    }
    fn pane(id: u32) -> PaneInfo {
        PaneInfo {
            id,
            title: format!("Terminal {id}"),
            ..PaneInfo::default()
        }
    }
    fn manifest(entries: &[(usize, Vec<PaneInfo>)]) -> PaneManifest {
        PaneManifest {
            panes: entries.iter().cloned().collect(),
        }
    }

    #[test]
    fn project_snapshots_preserve_repository_identity_ci_and_recovery() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3), pane(4)])]),
        );
        let wire = serde_json::json!({
            "name": "repo", "repository": true, "repository_id": "/repo/.git", "branch": "main",
            "branch_changes": {
                "base_ref": "refs/remotes/origin/main", "merge_base": "base",
                "total": {"added": 120, "deleted": 45, "binary_files": 1}
            },
            "ci": {
                "status": "running", "fetch_state": "ready",
                "checks": [{"name": "native tests", "status": "running", "url": null, "duration_seconds": null}],
                "change_request": {"number": 12, "title": "Native CI", "url": "https://github.com/owner/repo/pull/12", "state": "open"},
                "updated_at": 123, "stale": false, "error": null,
                "repository": {"provider": "github", "host": "github.com", "path": "owner/repo", "web_url": "https://github.com/owner/repo"}, "head_sha": "current",
                "branch": "main", "truncated": false
            }
        });
        for (id, cwd, repository_id) in [
            (1, "/repo", Some("/repo/.git")),
            (2, "/repo-linked", Some("/repo/.git")),
            (3, "/other/repo", Some("/other/repo/.git")),
            (4, "/directory", None),
        ] {
            model.panes.get_mut(&id).unwrap().cwd = Some(cwd.to_owned());
            let mut metadata = wire.clone();
            metadata["repository_id"] = serde_json::json!(repository_id);
            if repository_id.is_none() {
                metadata["repository"] = serde_json::json!(false);
                metadata["ci"] = serde_json::Value::Null;
                metadata["branch"] = serde_json::Value::Null;
                metadata["branch_changes"] = serde_json::Value::Null;
            }
            model
                .projects
                .insert(cwd.to_owned(), serde_json::from_value(metadata).unwrap());
        }
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].project, snapshot.panes[1].project);
        assert_ne!(snapshot.panes[0].project, snapshot.panes[2].project);
        assert_eq!(
            snapshot.panes[0].project.as_ref().unwrap().name,
            snapshot.panes[2].project.as_ref().unwrap().name
        );
        assert!(snapshot.panes[3].project.is_none());
        assert!(snapshot.panes[3].ci.is_none());
        assert!(snapshot.panes[3].branch_changes.is_none());
        let changes = snapshot.panes[0].branch_changes.as_ref().unwrap();
        assert_eq!(changes.total.added, 120);
        assert_eq!(changes.base_ref, "refs/remotes/origin/main");
        assert_eq!(changes.merge_base, "base");
        assert_eq!(snapshot.workspaces[0].projects.len(), 3);
        assert!(
            snapshot.workspaces[0]
                .projects
                .iter()
                .any(|p| !p.repository && p.id == "directory:/directory")
        );
        assert_eq!(
            snapshot.panes[0].ci.as_ref().unwrap().checks[0].name,
            "native tests"
        );
        let recovered: Model =
            serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(snapshot, recovered.snapshot(None, 1));
        let mut legacy = wire.clone();
        legacy.as_object_mut().unwrap().remove("branch_changes");
        assert!(
            serde_json::from_value::<Project>(legacy)
                .unwrap()
                .branch_changes
                .is_none()
        );

        let ticket = model.project_ticket("/repo", 99).unwrap();
        model.invalidate_project("/repo");
        assert!(!model.project_result_current("/repo", &ticket));
        let snapshot = model.snapshot(None, 1);
        assert!(snapshot.panes[0].project.is_none());
        assert!(snapshot.panes[0].branch.is_none());
        assert!(snapshot.panes[0].branch_changes.is_none());
        assert!(snapshot.panes[0].ci.is_none());
        assert_eq!(snapshot.panes[1].ci.as_ref().unwrap().head_sha, "current");
    }

    #[test]
    fn split_move_and_break_follow_stable_native_topology() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]),
        );
        let first = model.panes[&1].workspace.clone();
        let second = model.panes[&2].workspace.clone();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1), pane(3)]), (1, vec![pane(2)])]),
        );
        assert_eq!(model.panes[&3].workspace, first);
        model.reconcile(
            &[tab(20, 0), tab(10, 1)],
            &manifest(&[(0, vec![pane(2), pane(3)]), (1, vec![pane(1)])]),
        );
        assert_eq!(model.panes[&3].workspace, second);
        assert_eq!(model.panes[&1].workspace, first);
        model.reconcile(
            &[tab(20, 0), tab(10, 1), tab(30, 2)],
            &manifest(&[
                (0, vec![pane(2)]),
                (1, vec![pane(1)]),
                (2, vec![pane(3), pane(4)]),
            ]),
        );
        assert_eq!(model.panes[&3].workspace, second);
        assert_eq!(model.panes[&4].workspace, second);
        assert_eq!(model.panes[&3].tab, 30);
    }

    #[test]
    fn reload_retains_labels_and_held_terminals_until_close() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        let workspace = model.panes[&1].workspace.clone();
        model.workspaces.get_mut(&workspace).unwrap().name = "My workspace".to_owned();
        let terminal = model.panes.get_mut(&1).unwrap();
        terminal.name = "My terminal".to_owned();
        terminal.renamed = true;
        let mut recovered: Model =
            serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
        recovered.reconcile(
            &[tab(10, 0)],
            &manifest(&[(
                0,
                vec![
                    pane(1),
                    PaneInfo {
                        exited: true,
                        ..pane(2)
                    },
                ],
            )]),
        );
        assert_eq!(recovered.panes[&1].name, "My terminal");
        assert_eq!(recovered.workspaces[&workspace].name, "My workspace");
        assert!(recovered.panes.contains_key(&2));
        recovered.close_pane(2);
        assert!(!recovered.panes.contains_key(&2));
        recovered.reconcile(&[], &PaneManifest::default());
        assert!(recovered.workspaces.is_empty());
    }

    #[test]
    fn cached_owner_observes_other_replicas_and_refuses_corrupt_metadata() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "owner-cache-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.panes.get_mut(&1).unwrap().renamed = true;
        let mut first = Owner {
            configuration: model.configuration.clone(),
            model: Some(model),
            storage: directory.join("owner.json"),
            ..Owner::default()
        };
        let mut second = Owner {
            configuration: first.configuration.clone(),
            storage: first.storage.clone(),
            ..Owner::default()
        };
        first.save();
        second.recover_model();
        first
            .model
            .as_mut()
            .unwrap()
            .panes
            .get_mut(&1)
            .unwrap()
            .name = "First rename".to_owned();
        first.save();
        second.recover_model();
        assert_eq!(
            second.model.as_ref().unwrap().snapshot(None, 1).panes[0].name,
            "First rename"
        );
        second
            .model
            .as_mut()
            .unwrap()
            .panes
            .get_mut(&1)
            .unwrap()
            .name = "Other rename".to_owned();
        second.save();
        first.recover_model();
        assert_eq!(
            first.model.as_ref().unwrap().snapshot(None, 1).panes[0].name,
            "Other rename"
        );
        std::fs::write(&first.storage, b"{corrupt").unwrap();
        first.recover_model();
        assert!(first.error.is_some());
        assert_eq!(
            first.model.as_ref().unwrap().snapshot(None, 1).panes[0].name,
            "Other rename"
        );
        first.save();
        assert_eq!(std::fs::read(&first.storage).unwrap(), b"{corrupt");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_ci_cache_migrates_without_resetting_labels_or_current_metadata() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "ci-cache-migration-{}-{}",
                std::process::id(),
                clock_millis()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        let workspace = model.panes[&1].workspace.clone();
        model.workspaces.get_mut(&workspace).unwrap().name = "Persistent workspace".into();
        model.panes.get_mut(&1).unwrap().name = "Persistent terminal".into();
        model.panes.get_mut(&1).unwrap().renamed = true;
        model.observe_cwd(1, "/legacy".into());
        model.observe_cwd(2, "/current".into());
        assert!(model.sync_project_watches(5));
        let old_ticket = model.project_watches["/legacy"].clone();
        model.apply_project(project_update(
            &model,
            "/legacy",
            passing_project("legacy-sha"),
        ));
        model.apply_project(project_update(
            &model,
            "/current",
            passing_project("current-sha"),
        ));
        let current_ci = model.projects["/current"].ci.clone();
        let mut value = serde_json::to_value(&model).unwrap();
        value["projects"]["/legacy"]["ci"] = serde_json::json!({
            "status": "passed", "checks": [], "pr_number": 12, "pr_url": null,
            "updated_at": 123, "stale": false, "error": null,
            "host": "github.com", "repository": "owner/repo",
            "head_sha": "legacy-sha", "branch": "feature", "truncated": false
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut owner = Owner {
            configuration: model.configuration.clone(),
            storage: directory.join("owner.json"),
            ..Owner::default()
        };
        std::fs::write(&owner.storage, &bytes).unwrap();
        owner.recover_model();
        assert!(owner.error.is_none());
        assert_eq!(owner.storage_bytes, bytes);
        let migrated = owner.model.as_mut().unwrap();
        assert_eq!(migrated.workspaces[&workspace].name, "Persistent workspace");
        assert_eq!(migrated.panes[&1].name, "Persistent terminal");
        assert!(migrated.panes[&1].renamed);
        assert_eq!(
            migrated.projects["/legacy"].branch.as_deref(),
            Some("feature")
        );
        assert_eq!(
            migrated.projects["/legacy"].branch_changes,
            model.projects["/legacy"].branch_changes
        );
        assert!(migrated.projects["/legacy"].ci.is_none());
        assert_eq!(migrated.projects["/current"].ci, current_ci);
        assert!(!migrated.project_result_current("/legacy", &old_ticket));
        assert!(migrated.sync_project_watches(5));
        assert!(migrated.project_watches["/legacy"].generation > old_ticket.generation);
        assert!(migrated.apply_project(project_update(
            migrated,
            "/legacy",
            passing_project("refreshed-sha")
        )));
        owner.save();
        let canonical = owner.storage_bytes.clone();
        assert_ne!(canonical, bytes);
        assert_eq!(std::fs::read(&owner.storage).unwrap(), canonical);
        owner.model = None;
        owner.recover_model();
        assert!(owner.error.is_none());
        assert_eq!(
            owner.model.as_ref().unwrap().projects["/legacy"]
                .ci
                .as_ref()
                .unwrap()
                .head_sha,
            "refreshed-sha"
        );
        assert_eq!(
            owner.model.as_ref().unwrap().workspaces[&workspace].name,
            "Persistent workspace"
        );
        assert_eq!(owner.storage_bytes, canonical);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selection_relay_is_client_isolated_without_inventing_canonical_focus() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3)])]),
        );
        model.record_focus(1, 1);
        model.record_focus(2, 3);
        model.frontend_clients.insert(10, BTreeSet::from([1, 2]));
        model.frontend_clients.insert(20, BTreeSet::from([1]));
        let mut owner = Owner {
            client_id: 1,
            selection_epoch: "owner-first".into(),
            model: Some(model),
            ..Owner::default()
        };
        owner.select_ui("sidebar-one", 7, 2);
        let first = owner.state(1).unwrap();
        assert_eq!(
            first.snapshot.focused_pane_id.as_deref(),
            Some("terminal_1")
        );
        assert_eq!(
            first.ui_selection.as_ref().unwrap().terminal_id,
            "terminal_2"
        );
        assert_eq!(first.ui_selection.as_ref().unwrap().request_id, 7);

        assert_eq!(
            first.ui_selection.as_ref().unwrap().revision,
            first.ui_selection_revision
        );
        assert!(owner.state(2).unwrap().ui_selection.is_none());
        assert!(owner.state(2).unwrap().ui_selection_epoch.is_empty());
        assert_eq!(first.ui_selection_epoch, "owner-first");
        // Every registered replica for a host client receives the same transient state.
        let replicas: Vec<_> = owner
            .model
            .as_ref()
            .unwrap()
            .frontend_clients
            .iter()
            .filter(|(_, clients)| clients.contains(&1))
            .map(|(_, _)| serde_json::to_value(owner.state(1).unwrap()).unwrap())
            .collect();
        assert_eq!(replicas.len(), 2);
        assert_eq!(replicas[0], replicas[1]);
        owner.client_id = 2;
        owner.select_ui("sidebar-two", 1, 1);
        assert_eq!(owner.ui_selections.get(&1), first.ui_selection.as_ref());
        assert!(owner.state(1).unwrap().ui_selection_epoch.is_empty());
        let second = owner.state(2).unwrap();
        assert_eq!(
            second.snapshot.focused_pane_id.as_deref(),
            Some("terminal_3")
        );
        assert_eq!(second.ui_selection.as_ref().unwrap().client, "sidebar-two");
        assert!(second.ui_selection_revision > first.ui_selection_revision);
        let bytes = serde_json::to_vec(owner.model.as_ref().unwrap()).unwrap();
        let persisted = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        assert!(persisted.get("ui_selections").is_none());
        let restarted = Owner {
            client_id: 1,
            selection_epoch: "owner-restarted".into(),
            model: Some(Model::decode(&bytes).unwrap()),
            ..Owner::default()
        };
        assert!(restarted.state(1).unwrap().ui_selection.is_none());
        assert_ne!(
            restarted.state(1).unwrap().ui_selection_epoch,
            first.ui_selection_epoch
        );
        assert_eq!(restarted.state(1).unwrap().ui_selection_revision, 0);
        assert_eq!(
            restarted
                .state(1)
                .unwrap()
                .snapshot
                .focused_pane_id
                .as_deref(),
            Some("terminal_1")
        );
    }

    #[test]
    fn stale_focus_and_errors_cannot_clear_a_newer_selection_intent() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3)])]),
        );
        model.record_focus(1, 1);
        let mut owner = Owner {
            client_id: 1,
            model: Some(model),
            ..Owner::default()
        };
        owner.select_ui("sidebar", 1, 2);
        owner.select_ui("sidebar", 2, 3);
        let pending_revision = owner.state(1).unwrap().ui_selection_revision;
        owner.clear_selection(1, Some(("sidebar", 1)));
        owner.clear_selection(1, Some(("another-sidebar", 2)));
        owner.apply_host_focus(2, true);
        assert_eq!(
            owner
                .state(1)
                .unwrap()
                .ui_selection
                .as_ref()
                .unwrap()
                .request_id,
            2
        );
        assert_eq!(
            owner.state(1).unwrap().ui_selection_revision,
            pending_revision
        );
        // Read-side focus sampling does not acknowledge the host selection effect.
        owner.apply_host_focus(3, false);
        assert!(owner.state(1).unwrap().ui_selection.is_some());
        assert!(owner.apply_host_focus(3, true));
        let confirmed = owner.state(1).unwrap();
        assert!(confirmed.ui_selection.is_none());
        assert!(confirmed.ui_selection_revision > pending_revision);
        owner.select_ui("sidebar", 3, 2);
        let before_error = owner.state(1).unwrap().ui_selection_revision;
        owner.clear_selection(1, Some(("sidebar", 3)));
        assert!(owner.state(1).unwrap().ui_selection.is_none());
        assert!(owner.state(1).unwrap().ui_selection_revision > before_error);
        owner.select_ui("sidebar", 4, 2);
        let before_close = owner.state(1).unwrap().ui_selection_revision;
        owner.model.as_mut().unwrap().close_pane(2);
        owner.prune_selections();
        assert!(owner.state(1).unwrap().ui_selection.is_none());
        assert!(owner.state(1).unwrap().ui_selection_revision > before_close);
    }

    #[test]
    fn incomplete_tab_manifest_is_not_a_terminal_close() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let workspace = model.panes[&1].workspace.clone();
        model.workspaces.get_mut(&workspace).unwrap().name = "Persistent label".to_owned();
        assert!(!model.reconcile(&[tab(10, 0)], &PaneManifest::default()));
        assert_eq!(model.panes[&1].workspace, workspace);
        assert_eq!(model.workspaces[&workspace].name, "Persistent label");
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![])]));
        assert!(!model.panes.contains_key(&1));
    }

    #[test]
    fn shell_execution_is_idle_but_agent_or_shell_command_is_unknown() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let shell = vec!["/bin/bash".to_owned(), "-l".to_owned()];
        let terminal = model.panes.get_mut(&1).unwrap();
        terminal.shell = is_shell(&shell);
        terminal.command = shell;
        assert_eq!(model.snapshot(None, 0).panes[0].status, Status::Idle);
        for command in [
            vec!["claude".to_owned()],
            vec!["bash".to_owned(), "-c".to_owned(), "sleep 60".to_owned()],
        ] {
            let terminal = model.panes.get_mut(&1).unwrap();
            terminal.shell = is_shell(&command);
            terminal.command = command;
            assert_eq!(model.snapshot(None, 0).panes[0].status, Status::Unknown);
        }
    }

    #[test]
    fn topology_gate_preserves_breakout_until_the_matching_tab_report() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        let workspace = model.panes[&2].workspace.clone();
        let mut topology = Topology::default();
        topology.panes(manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]));
        assert!(topology.ready.take().is_none());
        assert_eq!(model.panes[&2].workspace, workspace);
        topology.tabs(vec![tab(10, 0), tab(20, 1)]);
        let (tabs, panes) = topology.ready.take().unwrap();
        model.reconcile(&tabs, &panes);
        assert_eq!(model.panes[&2].workspace, workspace);
        assert_eq!(model.panes[&2].tab, 20);
    }

    #[test]
    fn topology_gate_never_pairs_reordered_tabs_with_an_older_manifest() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]),
        );
        let first = model.panes[&1].workspace.clone();
        let second = model.panes[&2].workspace.clone();
        let mut topology = Topology::default();
        topology.panes(manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]));
        topology.tabs(vec![tab(10, 0), tab(20, 1)]);
        topology.panes(manifest(&[(0, vec![pane(2)]), (1, vec![pane(1)])]));
        assert!(topology.ready.take().is_none());
        topology.tabs(vec![tab(20, 0), tab(10, 1)]);
        let (tabs, panes) = topology.ready.take().unwrap();
        model.reconcile(&tabs, &panes);
        assert_eq!(model.panes[&1].workspace, first);
        assert_eq!(model.panes[&2].workspace, second);
    }

    #[test]
    fn client_focus_is_independent_per_attached_client() {
        let mut model = model();
        let sidebar = PaneInfo {
            id: 99,
            is_plugin: true,
            ..PaneInfo::default()
        };
        let panes = manifest(&[(0, vec![pane(1)]), (1, vec![pane(2), sidebar])]);
        model.reconcile(&[tab(10, 0), tab(20, 1)], &panes);
        model.record_focus(1, 1);
        model.record_focus(2, 2);
        assert_eq!(
            model.snapshot(None, 1).focused_pane_id.as_deref(),
            Some("terminal_1")
        );
        assert_eq!(
            model.snapshot(None, 2).focused_pane_id.as_deref(),
            Some("terminal_2")
        );
    }

    #[test]
    fn project_generation_rejects_invalidated_and_other_owner_results() {
        let mut model = model();
        let stale = model.project_ticket("/repository", 5).unwrap();
        assert!(model.project_ticket("/repository", 5).is_none());
        model.invalidate_project("/repository");
        assert!(!model.project_result_current("/repository", &stale));
        let replacement = model.project_ticket("/repository", 5).unwrap();
        assert!(model.project_result_current("/repository", &replacement));
        let new_owner = model.project_ticket("/repository", 6).unwrap();
        assert!(!model.project_result_current("/repository", &replacement));
        assert!(model.project_result_current("/repository", &new_owner));
    }

    #[test]
    fn created_tab_binding_survives_an_older_coherent_report() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let created = model.workspace("Created workspace".to_owned());
        model.tabs.insert(20, created.clone());
        model.pending_tabs.insert(20);
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        assert_eq!(model.workspaces[&created].name, "Created workspace");
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]),
        );
        assert_eq!(model.panes[&2].workspace, created);
    }

    #[test]
    fn confirmed_close_cannot_be_resurrected_by_a_queued_old_report() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        model.record_focus(1, 1);
        model.close_pane(1);
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        assert!(!model.panes.contains_key(&1));
        assert_eq!(model.snapshot(None, 1).focused_pane_id, None);
        assert_eq!(model.snapshot(None, 1).panes[0].id, "terminal_2");
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(2)])]));
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        assert!(!model.panes.contains_key(&1));
    }

    #[test]
    fn fresh_stock_tab_does_not_adopt_unrelated_workspace() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let first = model.panes[&1].workspace.clone();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]),
        );
        assert_ne!(model.panes[&2].workspace, first);
        assert_eq!(model.workspaces[&model.panes[&2].workspace].name, "Tab 20");
    }

    fn execution(id: u32) -> AgentExecution {
        AgentExecution {
            pane_id: id,
            root_pid: 100 + id as i32,
            pid: 200 + id as i32,
            identity: format!("birth-{id}"),
            harness: "claude".to_owned(),
            cwd: None,
        }
    }

    fn inspect(model: &mut Model, executions: Vec<AgentExecution>, now: u64) {
        let ticket = model.agent_ticket(5, 1, now).unwrap();
        model
            .apply_agents(&ticket, InspectResponse { executions }, now + 1)
            .unwrap();
    }

    fn passing_project(sha: &str) -> Project {
        serde_json::from_value(serde_json::json!({
            "name": "repo", "repository": true, "repository_id": "/repo/.git", "branch": "feature",
            "branch_changes": {
                "base_ref": "refs/remotes/origin/main", "merge_base": "base",
                "total": {"added": 12, "deleted": 3, "binary_files": 0}
            },
            "ci": {
                "status": "passed", "fetch_state": "ready",
                "checks": [{"name": "tests", "status": "passed", "url": null, "duration_seconds": 42}],
                "change_request": {"number": 12, "title": "Native CI", "url": "https://github.com/owner/repo/pull/12", "state": "open"},
                "updated_at": 123, "stale": false, "error": null,
                "repository": {"provider": "github", "host": "github.com", "path": "owner/repo", "web_url": "https://github.com/owner/repo"}, "head_sha": sha,
                "branch": "feature", "truncated": false
            }
        }))
        .unwrap()
    }

    fn project_update(model: &Model, cwd: &str, project: Project) -> ProjectUpdate {
        ProjectUpdate {
            cwd: cwd.to_owned(),
            ticket: model.project_watches[cwd].clone(),
            project: Some(project),
            error: None,
        }
    }

    #[test]
    fn ci_refresh_requires_registered_client_and_current_watched_commit_without_reticketing() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_cwd(1, "/repo".into());
        model.sync_project_watches(5);
        model.apply_project(project_update(&model, "/repo", passing_project("old")));
        let ci = model.projects["/repo"].ci.as_ref().unwrap();
        let target = CiRefreshTarget {
            repository: ci.repository.clone(),
            head_sha: ci.head_sha.clone(),
        };
        assert!(!model.ci_refresh_current(7, 1, &target));
        model.frontends.insert(7);
        model.frontend_clients.insert(7, BTreeSet::from([1]));
        let sequence = model.project_watch_sequence;
        let watches = model.project_watches.clone();
        assert!(model.ci_refresh_current(7, 1, &target));
        assert!(!model.ci_refresh_current(7, 2, &target));
        assert!(!model.ci_refresh_current(8, 1, &target));
        let mut forged = target.clone();
        forged.repository.host = "other.example".into();
        assert!(!model.ci_refresh_current(7, 1, &forged));
        assert_eq!(model.project_watch_sequence, sequence);
        assert!(model.project_watches == watches);
        let stale = project_update(&model, "/repo", passing_project("old"));
        model.apply_project(project_update(&model, "/repo", passing_project("new")));
        assert!(!model.ci_refresh_current(7, 1, &target));
        model.invalidate_project("/repo");
        assert!(!model.apply_project(stale));
        assert!(!model.ci_refresh_current(7, 1, &target));
    }

    #[test]
    fn typed_ci_refresh_envelope_roundtrips_without_changing_snapshot_protocol() {
        let ci = passing_project("head").ci.unwrap();
        let envelope = NativeCiRefresh {
            client_id: 1,
            target: CiRefreshTarget {
                repository: ci.repository,
                head_sha: ci.head_sha,
            },
        };
        let encoded = serde_json::to_string(&envelope).unwrap();
        let decoded: NativeCiRefresh = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.client_id, 1);
        assert_eq!(decoded.target, envelope.target);
    }

    #[test]
    fn repeated_harness_lifecycles_preserve_directory_metadata_and_subscription() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_cwd(1, "/repo".into());
        model.observe_roots(BTreeMap::from([(1, 101)]));
        model.observe_command(1, vec!["bash".into()]);
        assert!(model.sync_project_watches(5));
        assert!(model.apply_project(project_update(&model, "/repo", passing_project("first"))));
        let ticket = model.project_watches["/repo"].clone();
        let sequence = model.project_watch_sequence;

        for run in 0..4 {
            let now = 10_000 + run * 10_000;
            let stale_agent = model.agent_ticket(5, 1, now).unwrap();
            assert!(model.observe_command(1, vec!["claude".into()]));
            assert!(!model.agent_result_current(&stale_agent, now + 1));
            let root = 101 + run as i32;
            model.observe_roots(BTreeMap::from([(1, root)]));
            let mut agent = execution(1);
            agent.root_pid = root;
            agent.pid += run as i32;
            agent.identity = format!("run-{run}");
            agent.cwd = Some("/repo".into());
            inspect(&mut model, vec![agent], now + AGENT_RESULT_MILLIS + 2);
            model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: vec![program("working", "claude-code")],
                })
                .unwrap();
            assert!(!model.sync_project_watches(5));
            let active = &model.snapshot(None, 1).panes[0];
            assert_eq!(active.branch.as_deref(), Some("feature"));
            assert_eq!(active.branch_changes.as_ref().unwrap().total.added, 12);
            assert_eq!(active.ci.as_ref().unwrap().head_sha, "first");
            assert_eq!(
                active.ci.as_ref().unwrap().status,
                shephrd::ci_contract::CiStatus::Passed
            );

            model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: Vec::new(),
                })
                .unwrap();
            assert!(model.observe_command(1, vec!["bash".into()]));
            inspect(&mut model, Vec::new(), now + AGENT_RESULT_MILLIS + 3);
            assert!(!model.sync_project_watches(5));
            let shell = &model.snapshot(None, 1).panes[0];
            assert_eq!(shell.harness, None);
            assert_eq!(shell.status, Status::Idle);
            assert_eq!(shell.branch.as_deref(), Some("feature"));
            assert_eq!(shell.branch_changes.as_ref().unwrap().total.deleted, 3);
            assert_eq!(
                shell.ci.as_ref().unwrap().status,
                shephrd::ci_contract::CiStatus::Passed
            );
            assert_eq!(model.project_watch_sequence, sequence);
            assert!(model.project_watches["/repo"] == ticket);
            assert!(model.project_result_current("/repo", &ticket));
        }
    }

    #[test]
    fn shared_cwd_switch_close_recovery_and_commit_updates_keep_current_projects_only() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3)])]),
        );
        model.observe_cwd(1, "/repo".into());
        model.observe_cwd(2, "/repo".into());
        model.observe_cwd(3, "/other".into());
        assert!(model.sync_project_watches(5));
        model.apply_project(project_update(
            &model,
            "/repo",
            passing_project("repo-head"),
        ));
        model.apply_project(project_update(
            &model,
            "/other",
            passing_project("other-head"),
        ));
        let stale = project_update(&model, "/repo", passing_project("repo-head"));
        assert!(model.observe_cwd(1, "/other".into()));
        assert!(!model.sync_project_watches(5));
        let snapshot = model.snapshot(None, 1);
        assert_eq!(
            snapshot.panes[0].ci.as_ref().unwrap().head_sha,
            "other-head"
        );
        assert_eq!(snapshot.panes[1].ci.as_ref().unwrap().head_sha, "repo-head");
        assert_eq!(
            snapshot.panes[1]
                .branch_changes
                .as_ref()
                .unwrap()
                .total
                .added,
            12
        );

        assert!(model.close_pane(2));
        assert!(model.sync_project_watches(5));
        assert!(!model.projects.contains_key("/repo"));
        assert!(model.observe_cwd(1, "/repo".into()));
        assert!(model.sync_project_watches(5));
        assert!(!model.apply_project(stale));
        assert!(model.snapshot(None, 1).panes[0].ci.is_none());
        model.apply_project(project_update(
            &model,
            "/repo",
            passing_project("next-head"),
        ));

        let mut recovered: Model =
            serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
        let previous_owner = project_update(&recovered, "/repo", passing_project("repo-head"));
        assert!(recovered.sync_project_watches(6));
        assert_eq!(
            recovered.snapshot(None, 1).panes[0]
                .ci
                .as_ref()
                .unwrap()
                .head_sha,
            "next-head"
        );
        assert!(!recovered.apply_project(previous_owner));
        let mut changed_commit = passing_project("new-head");
        changed_commit.ci = None;
        assert!(recovered.apply_project(project_update(&recovered, "/repo", changed_commit)));
        let snapshot = recovered.snapshot(None, 1);
        assert!(snapshot.panes[0].ci.is_none());
        assert_eq!(
            snapshot.panes[1].ci.as_ref().unwrap().head_sha,
            "other-head"
        );
        assert_eq!(recovered.projects.len(), 2);
        let missing_directory = ProjectUpdate {
            cwd: "/repo".into(),
            ticket: recovered.project_watches["/repo"].clone(),
            project: None,
            error: Some("project cwd must be an existing absolute directory".into()),
        };
        assert!(recovered.apply_project(missing_directory));
        let snapshot = recovered.snapshot(None, 1);
        assert!(snapshot.panes[0].project.is_none());
        assert!(snapshot.panes[0].branch.is_none());
        assert!(snapshot.panes[0].ci.is_none());
        assert_eq!(
            snapshot.panes[1].ci.as_ref().unwrap().head_sha,
            "other-head"
        );
        assert!(recovered.apply_project(project_update(
            &recovered,
            "/repo",
            passing_project("recovered-head")
        )));
        assert!(!recovered.failed_projects.contains("/repo"));
    }

    #[test]
    fn workspace_program_aggregation_prefers_attention_then_active_work_over_completion() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("done", "claude-code")],
            })
            .unwrap();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("working", "omp")],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].status,
            Status::Working
        );
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("blocked", "claude-code")],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].status,
            Status::Blocked
        );
    }

    #[test]
    fn inactive_agent_metadata_updates_without_overwriting_editable_names() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)])]),
        );
        model.record_focus(1, 1);
        model.observe_roots(BTreeMap::from([(1, 101), (2, 102)]));
        model.panes.get_mut(&2).unwrap().name = "User label".to_owned();
        model.panes.get_mut(&2).unwrap().renamed = true;
        let mut agent = execution(2);
        agent.cwd = Some("/project".to_owned());
        inspect(&mut model, vec![agent.clone()], 1_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.focused_pane_id.as_deref(), Some("terminal_1"));
        let pane = &snapshot.panes[1];
        assert_eq!(pane.name, "User label");
        assert_eq!(pane.harness.as_deref(), Some("claude"));
        assert_eq!(pane.cwd.as_deref(), Some("/project"));
        assert_eq!(pane.status, Status::Unknown);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("working", "claude-code")],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[1].status, Status::Working);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("done", "claude-code")],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[1].status, Status::Done);
    }

    #[test]
    fn agent_inspection_is_single_flight_across_clients_and_deadlines_reject_old_results() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let first = model.agent_ticket(5, 1, 1_000).unwrap();
        assert!(model.agent_ticket(5, 2, 1_001).is_none());
        assert!(model.agent_ticket(6, 1, 1_001).is_none());
        assert!(model.agent_result_current(&first, 5_999));
        assert!(!model.agent_result_current(&first, 6_000));
        let replacement = model.agent_ticket(5, 2, 6_000).unwrap();
        assert!(!model.agent_result_current(&first, 6_001));
        assert!(model.agent_result_current(&replacement, 6_001));
        assert!(
            model
                .apply_agents(
                    &first,
                    InspectResponse {
                        executions: vec![execution(1)],
                    },
                    6_001
                )
                .is_err()
        );
        assert!(model.pending_agents.as_ref() == Some(&replacement));
    }

    #[test]
    fn closed_moved_or_replaced_roots_reject_in_flight_agent_metadata() {
        for transition in 0..3 {
            let mut model = model();
            model.reconcile(
                &[tab(10, 0), tab(20, 1)],
                &manifest(&[(0, vec![pane(1)]), (1, vec![])]),
            );
            model.observe_roots(BTreeMap::from([(1, 101)]));
            inspect(&mut model, vec![execution(1)], 1_000);
            let stale = model.agent_ticket(5, 1, 2_000).unwrap();
            match transition {
                0 => {
                    model.close_pane(1);
                }
                1 => {
                    model.reconcile(
                        &[tab(10, 0), tab(20, 1)],
                        &manifest(&[(0, vec![]), (1, vec![pane(1)])]),
                    );
                }
                _ => {
                    model.observe_roots(BTreeMap::from([(1, 999)]));
                }
            }
            assert!(
                model
                    .apply_agents(
                        &stale,
                        InspectResponse {
                            executions: vec![execution(1)],
                        },
                        2_001
                    )
                    .is_err()
            );
            if transition == 1 {
                assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
            } else {
                assert!(model.agents.is_empty());
            }
        }
    }

    #[test]
    fn disappearing_execution_clears_agent_and_replacement_has_no_semantic_status() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        model.observe_command(1, vec!["bash".to_owned()]);
        let old = execution(1);
        inspect(&mut model, vec![old.clone()], 1_000);
        let ticket = model.agent_ticket(5, 1, 2_000).unwrap();
        let mut replacement = old;
        replacement.pid = 500;
        replacement.identity = "new-birth".to_owned();
        model
            .apply_agents(
                &ticket,
                InspectResponse {
                    executions: vec![replacement],
                },
                2_001,
            )
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        inspect(&mut model, vec![], 3_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness, None);
        assert_eq!(snapshot.panes[0].status, Status::Idle);
        assert_eq!(snapshot.panes[0].description, "Native shell · bash");
    }

    #[test]
    fn inspector_conflicts_are_atomic_and_command_return_drops_execution_metadata() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        inspect(&mut model, vec![execution(1)], 1_000);
        let ticket = model.agent_ticket(5, 1, 2_000).unwrap();
        assert!(
            model
                .apply_agents(
                    &ticket,
                    InspectResponse {
                        executions: vec![execution(1), execution(1)],
                    },
                    2_001
                )
                .is_err()
        );
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
        model.observe_command(1, vec!["bash".to_owned()]);
        assert_eq!(model.snapshot(None, 1).panes[0].harness, None);
        assert!(!model.agent_result_current(&ticket, 2_002));
    }

    fn program(state: &str, app: &str) -> ProgramStatus {
        ProgramStatus {
            id: String::new(),
            state: state.to_owned(),
            kind: None,
            progress: None,
            app: Some(app.to_owned()),
            title: None,
            msg: None,
        }
    }

    fn inspect_programs(
        model: &mut Model,
        executions: Vec<AgentExecution>,
        records: Vec<ProgramStatus>,
        now: u64,
    ) {
        inspect(model, executions, now);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records,
            })
            .unwrap();
    }

    #[test]
    fn native_task_title_updates_while_reporting_and_survives_report_clear() {
        let mut model = model();
        let mut terminal = pane(1);
        terminal.title = "First task".to_owned();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal.clone()])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let mut agent = execution(1);
        agent.harness = "omp".to_owned();
        inspect_programs(
            &mut model,
            vec![agent.clone()],
            vec![program("working", "omp")],
            1_000,
        );
        assert_eq!(model.snapshot(None, 1).panes[0].name, "First task");
        terminal.title = "Live task update".to_owned();
        assert!(model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal])])));
        inspect(&mut model, vec![agent], 2_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].name, "Live task update");
        assert_eq!(snapshot.panes[0].status, Status::Working);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("omp"));
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![],
            })
            .unwrap();
        model.observe_roots(BTreeMap::new());
        assert_eq!(model.snapshot(None, 1).panes[0].name, "Live task update");
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
    }

    #[test]
    fn native_title_priority_preserves_structured_and_explicit_labels() {
        let mut model = model();
        let mut terminal = pane(1);
        terminal.title.clear();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal.clone()])]));
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("working", "omp")],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].name, "omp");
        terminal.title = "Native task".to_owned();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal.clone()])]));
        assert_eq!(model.snapshot(None, 1).panes[0].name, "Native task");
        let mut record = program("working", "omp");
        record.title = Some("Structured task".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![record],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].name, "Structured task");
        model.panes.get_mut(&1).unwrap().name = "User label".to_owned();
        model.panes.get_mut(&1).unwrap().renamed = true;
        terminal.title = "Later native task".to_owned();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal])]));
        assert_eq!(model.snapshot(None, 1).panes[0].name, "User label");
        assert!(model.panes[&1].has_native_title);
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Working);
    }

    #[test]
    fn native_run_states_remain_authoritative_across_execution_changes() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        for (state, kind, status, attention) in [
            ("idle", None, Status::Idle, None),
            ("working", None, Status::Working, None),
            (
                "blocked",
                Some("question"),
                Status::Blocked,
                Some(Attention::Question),
            ),
            (
                "blocked",
                Some("permission"),
                Status::Blocked,
                Some(Attention::Permission),
            ),
            ("error", None, Status::Blocked, Some(Attention::Error)),
            ("done", None, Status::Done, None),
        ] {
            let ticket = model.agent_ticket(5, 1, 1_000).unwrap();
            let mut record = program(state, "omp");
            record.kind = kind.map(str::to_owned);
            model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: vec![record],
                })
                .unwrap();
            model
                .apply_agents(
                    &ticket,
                    InspectResponse {
                        executions: vec![execution(1)],
                    },
                    1_001,
                )
                .unwrap();
            model.observe_command(1, vec![state.to_owned()]);
            let snapshot = model.snapshot(None, 1);
            assert_eq!(snapshot.panes[0].status, status, "{state}");
            assert_eq!(snapshot.panes[0].attention, attention, "{state}");
            assert_eq!(snapshot.panes[0].harness.as_deref(), Some("omp"));
        }
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
    }

    #[test]
    fn pushed_status_wins_in_flight_inspection_and_survives_execution_exit() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let ticket = model.agent_ticket(5, 1, 1_000).unwrap();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("working", "cargo")],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Working);
        model
            .apply_agents(
                &ticket,
                InspectResponse {
                    executions: vec![execution(1)],
                },
                1_001,
            )
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Working);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("done", "cargo")],
            })
            .unwrap();
        model.observe_roots(BTreeMap::new());
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Done);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("cargo"));
    }

    #[test]
    fn pushed_records_wait_for_topology_but_closed_terminals_stay_closed() {
        let mut model = model();
        let update = ProgramStatusUpdate {
            pane_id: 1,
            records: vec![program("working", "cargo")],
        };
        model.apply_program_status(update.clone()).unwrap();
        model.reconcile(&[], &PaneManifest::default());
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Working);
        model.close_pane(1);
        model.reconcile(&[], &PaneManifest::default());
        model.apply_program_status(update).unwrap();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        assert!(model.snapshot(None, 1).panes.is_empty());
        assert!(!model.program_status.contains_key(&1));

        let unseen = ProgramStatusUpdate {
            pane_id: 2,
            records: vec![program("done", "cargo")],
        };
        model.apply_program_status(unseen).unwrap();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![],
            })
            .unwrap();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(2)])]));
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
    }

    #[test]
    fn malformed_pushed_records_leave_current_terminal_status_intact() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let valid = program("done", "cargo");
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![valid.clone()],
            })
            .unwrap();
        let mut oversize = valid.clone();
        oversize.msg = Some("a".repeat(2049));
        let mut invalid_state = valid.clone();
        invalid_state.state = "clear".to_owned();
        let mut invalid_progress = valid.clone();
        invalid_progress.progress = Some(101);
        for records in [
            vec![valid.clone(), valid.clone()],
            vec![valid; 257],
            vec![oversize],
            vec![invalid_state],
            vec![invalid_progress],
        ] {
            assert!(
                model
                    .apply_program_status(ProgramStatusUpdate {
                        pane_id: 1,
                        records
                    })
                    .is_err()
            );
            assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Done);
        }
    }

    #[test]
    fn structured_records_aggregate_children_and_clear_to_unknown_without_reports() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let agent = execution(1);
        let mut root = program("working", "claude-code");
        root.title = Some("Structured title".to_owned());
        inspect_programs(&mut model, vec![agent.clone()], vec![root.clone()], 1_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Working);
        assert_eq!(snapshot.panes[0].name, "Structured title");
        let mut hidden = program("done", "claude-code");
        hidden.id = "background".to_owned();
        assert!(
            !model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: vec![root.clone(), hidden],
                })
                .unwrap()
        );
        assert_eq!(model.snapshot(None, 1), snapshot);
        let mut child = program("error", "claude-code");
        child.id = "worker".to_owned();
        child.msg = Some("Build failed".to_owned());
        inspect_programs(&mut model, vec![agent.clone()], vec![root, child], 2_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Blocked);
        assert_eq!(snapshot.workspaces[0].status, Status::Blocked);
        model.panes.get_mut(&1).unwrap().name = "Explicit label".to_owned();
        model.panes.get_mut(&1).unwrap().renamed = true;
        assert_eq!(model.snapshot(None, 1).panes[0].name, "Explicit label");
        inspect_programs(&mut model, vec![agent], vec![], 3_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].name, "Explicit label");
    }

    #[test]
    fn omp_native_title_decoration_leaves_task_text_intact() {
        for prefix in [
            "π > ", "π ! ", "π : ", "π: ", "π ⠋ ", "π ⠴ ", "π ◑ ", "π / ",
        ] {
            assert_eq!(
                native_title(&format!("{prefix}Fix π > 日本語"), Some("omp")),
                "Fix π > 日本語"
            );
        }
        for title in ["π task", "π >task", "Task > name", "π", "Task"] {
            assert_eq!(native_title(title, Some("omp")), title);
        }
        assert_eq!(native_title("π > Task", Some("claude")), "π > Task");
    }

    #[test]
    fn generic_completed_status_survives_execution_exit_without_borrowing_next_task() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        model.observe_command(1, vec!["bash".to_owned()]);
        let mut cargo = program("done", "cargo");
        cargo.title = Some("Compile\u{202e}\u{200b} project".to_owned());
        cargo.msg = Some("Compiled successfully".to_owned());
        inspect_programs(&mut model, vec![], vec![cargo.clone()], 1_000);
        assert_eq!(model.snapshot(None, 1).panes[0].name, "Compile project");
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Done);
        model.observe_roots(BTreeMap::new());
        inspect_programs(&mut model, vec![], vec![cargo.clone()], 2_000);
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Done);
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let next = execution(1);
        inspect_programs(&mut model, vec![next.clone()], vec![cargo], 6_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].name, "Compile project");
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("cargo"));
        inspect_programs(&mut model, vec![next], vec![], 7_000);
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
    }

    #[test]
    fn execution_metadata_is_invalidated_by_lifecycle_not_idle_time() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        inspect(&mut model, vec![execution(1)], 1_000);
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
        model.observe_command(1, vec!["bash".to_owned()]);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness, None);
        assert_eq!(snapshot.panes[0].status, Status::Idle);
    }

    #[test]
    fn nondominant_attention_changes_publish_the_new_aggregate() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let mut root = program("blocked", "claude-code");
        root.kind = Some("permission".to_owned());
        let mut hidden = root.clone();
        hidden.id = "a".to_owned();
        hidden.kind = Some("question".to_owned());
        let mut dominant = root.clone();
        dominant.id = "z".to_owned();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![root.clone(), hidden.clone(), dominant.clone()],
            })
            .unwrap();
        let before = model.snapshot(None, 1);
        assert_eq!(before.panes[0].status, Status::Blocked);
        assert_eq!(before.panes[0].attention, None);
        assert_eq!(before.workspaces[0].attention, None);

        hidden.kind = Some("permission".to_owned());
        assert!(
            model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: vec![root, hidden, dominant],
                })
                .unwrap()
        );
        let after = model.snapshot(None, 1);
        assert_eq!(after.panes[0].attention, Some(Attention::Permission));
        assert_eq!(after.workspaces[0].attention, Some(Attention::Permission));
    }

    #[test]
    fn workspace_attention_tracks_mixed_unspecified_cleared_and_resumed_reports() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        let mut permission = program("blocked", "claude-code");
        permission.kind = Some("permission".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![permission],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].attention,
            Some(Attention::Permission)
        );

        let mut question = program("blocked", "omp");
        question.kind = Some("question".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![question],
            })
            .unwrap();
        let mixed = model.snapshot(None, 1);
        assert_eq!(mixed.panes[1].attention, Some(Attention::Question));
        assert_eq!(mixed.workspaces[0].status, Status::Blocked);
        assert_eq!(mixed.workspaces[0].attention, None);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("blocked", "omp")],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).workspaces[0].attention, None);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].attention,
            Some(Attention::Permission)
        );

        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![],
            })
            .unwrap();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("error", "omp")],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].attention,
            Some(Attention::Error)
        );
        let mut auth = program("blocked", "omp");
        auth.kind = Some("auth".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![auth],
            })
            .unwrap();
        assert_eq!(
            model.snapshot(None, 1).workspaces[0].attention,
            Some(Attention::Auth)
        );
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("working", "omp")],
            })
            .unwrap();
        let resumed = model.snapshot(None, 1);
        assert_eq!(resumed.panes[1].attention, None);
        assert_eq!(resumed.workspaces[0].attention, None);
        assert_eq!(resumed.workspaces[0].status, Status::Working);
    }
}
