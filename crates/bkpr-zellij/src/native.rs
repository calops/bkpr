use bkpr::agent_contract::{
    AgentExecution, InspectRequest, InspectResponse, PaneProcess, ProgramStatus,
    ProgramStatusUpdate, harness_name,
};
use bkpr::ci_contract::{CiRefreshTarget, CiSnapshot};
use bkpr::context_contract::{ContextPane, ContextPublication, ContextWorkspace, PaneStatus};
use bkpr::recovery_contract::{
    GeometryPane, RECOVERY_COALESCE_MILLIS, RECOVERY_REMOVAL_GRACE_MILLIS, RecoveryPane,
    RecoveryTab, RecoveryUpdate, RecoveryWorkspace, RestoreLink, SplitNode, split_tree,
};
use bkpr::{
    Action, Attention, Effect, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Pane, ProjectIdentity, Request,
    Response, Snapshot, Status, Workspace,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zellij_tile::prelude::*;

mod control;
pub(crate) use control::{expire_control_ready, settle_control_ready, sidebar_control_pipe};
pub const CONTROL: &str = "bkpr-control";

pub const REQUEST: &str = "bkpr-native-request";
pub const RESPONSE: &str = "bkpr-native-response";
pub const STATE: &str = "bkpr-native-state";
pub const QUERY: &str = "bkpr-native-query";
pub const PROJECT: &str = "bkpr-native-project";
pub const REFRESH_CI: &str = "bkpr-native-refresh-ci";
/// CLI pipe from `bkpr restore-pane`: `{"key": <leaf key>, "pane_id": "terminal_N"}`, plus
/// `"harness"`/`"agent_id"` when the pane's recorded association is kept for a retry.
pub const RESTORE_NAME: &str = "bkpr-restore-name";
#[derive(Serialize, Deserialize)]
struct NativeCiRefresh {
    client_id: u16,
    target: CiRefreshTarget,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct HostContextTab {
    tab_id: usize,
    workspace_id: String,
    workspace_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct HostContextPane {
    pane_id: String,
    tab_id: usize,
    workspace_id: String,
    workspace_name: String,
    #[serde(default)]
    root_pid: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct HostContext {
    version: u32,
    session_id: String,
    session_name: String,
    tabs: Vec<HostContextTab>,
    panes: Vec<HostContextPane>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct ContextProjection {
    session_name: String,
    workspaces: Vec<ContextWorkspace>,
    panes: Vec<ContextPane>,
}

#[derive(Serialize, Deserialize)]
pub struct NativeRequest {
    pub client: String,
    pub client_id: u16,
    pub request: Request,
    #[serde(default)]
    pub return_mode: Option<InputMode>,
    #[serde(default)]
    pub view: Option<SidebarView>,
}

#[derive(Serialize)]
struct NativeRequestRef<'a> {
    client: &'a str,
    client_id: u16,
    request: &'a Request,
    return_mode: Option<InputMode>,
    view: Option<&'a SidebarView>,
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

/// A creation-time projection, never an acknowledgement of the live owner stream.
#[derive(Deserialize)]
pub struct SidebarBootstrap {
    pub snapshot: Snapshot,
    pub mode: InputMode,
    pub base_mode: Option<InputMode>,
    pub keymap: KeymapCacheRef,
    #[serde(default)]
    pub view: SidebarView,
}

#[derive(Default, Deserialize, Serialize)]
pub struct SidebarView {
    pub collapsed: BTreeSet<String>,
    pub scroll: usize,
}

#[cfg(not(test))]
const KEYMAP_CACHE_DIRECTORY: &str = "/cache";
#[cfg(test)]
const KEYMAP_CACHE_DIRECTORY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/sidebar-keymap-cache"
);

/// Immutable, owner/client-scoped keymap publication, separate from per-tab configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct KeymapCacheRef {
    instance_id: String,
    source: String,
    client_id: u16,
    generation: u64,
    content_hash: u64,
}

#[derive(Deserialize, Serialize)]
struct KeymapCache {
    reference: KeymapCacheRef,
    payload: String,
}

fn keymap_hash(payload: &str) -> u64 {
    payload.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

impl KeymapCacheRef {
    fn path(&self) -> Option<PathBuf> {
        if self.source.is_empty()
            || self.source.len() > 128
            || !self
                .source
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b':')
        {
            return None;
        }
        Some(Path::new(KEYMAP_CACHE_DIRECTORY).join(format!(
            "bkpr-keymap-{}-{}.json",
            self.source, self.generation
        )))
    }

    pub fn load(&self, instance_id: &str, client_id: u16) -> Option<(KeybindsVec, String)> {
        if self.instance_id != instance_id || self.client_id != client_id {
            return None;
        }
        let file = std::fs::File::open(self.path()?).ok()?;
        let mut bytes = Vec::new();
        file.take((MAX_MESSAGE_BYTES * 2 + 4096 + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_MESSAGE_BYTES * 2 + 4096 {
            return None;
        }
        let cache: KeymapCache = serde_json::from_slice(&bytes).ok()?;
        if cache.reference != *self
            || cache.payload.len() > MAX_MESSAGE_BYTES
            || keymap_hash(&cache.payload) != self.content_hash
        {
            return None;
        }
        let keybinds = serde_json::from_str(&cache.payload).ok()?;
        Some((keybinds, cache.payload))
    }
}

#[cfg(test)]
pub fn test_keymap_cache(instance_id: &str, keybinds: &KeybindsVec) -> KeymapCacheRef {
    std::fs::create_dir_all(KEYMAP_CACHE_DIRECTORY).unwrap();
    let mut owner = Owner {
        model: Some(Model::new(BTreeMap::new(), instance_id.to_owned())),
        selection_epoch: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string(),
        ..Owner::default()
    };
    owner.cache_keymap(serde_json::to_string(keybinds).unwrap());
    owner.keymap_cache.unwrap()
}

#[derive(Clone, Copy, Deserialize)]
struct GuideMode {
    mode: InputMode,
    base_mode: Option<InputMode>,
}

#[derive(Serialize)]
struct SidebarBootstrapRef<'a> {
    snapshot: Snapshot,
    mode: InputMode,
    base_mode: Option<InputMode>,
    keymap: &'a KeymapCacheRef,
    view: &'a SidebarView,
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
            self.ready = None;
            self.reconciled = None;
            return false;
        };
        let positions: BTreeSet<_> = tabs.iter().map(|tab| tab.position).collect();
        if positions.len() != tabs.len() || positions != manifest.panes.keys().copied().collect() {
            self.pending = Some(manifest);
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

    fn sidebar_percent(&self, source: u32) -> Result<u8, String> {
        if self.pending.is_some() {
            return Err("Native tab geometry is updating; try creating the tab again".to_owned());
        }
        let (tabs, manifest) = self
            .ready
            .as_ref()
            .or(self.reconciled.as_ref())
            .ok_or("Native tab geometry is unavailable")?;
        for tab in tabs {
            let Some(panes) = manifest.panes.get(&tab.position) else {
                continue;
            };
            if let Some(pane) = panes
                .iter()
                .find(|pane| pane.is_plugin && pane.id == source)
            {
                if pane.is_floating || pane.is_suppressed || pane.exited {
                    return Err("Tab creation requires an open tiled sidebar".to_owned());
                }
                if pane.pane_columns > tab.display_area_columns {
                    return Err("Native sidebar geometry exceeds the display".to_owned());
                }
                // Outer columns include the separator; content columns would shrink each tab.
                return bkpr::native_sidebar_percent(pane.pane_columns, tab.display_area_columns)
                    .ok_or_else(|| "Native sidebar geometry is unavailable".to_owned());
            }
        }
        Err("The requesting sidebar is no longer open".to_owned())
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

/// A `session-record` helper that never reports back (crash, lost callback) is treated as
/// failed after this long, so recovery cannot wedge behind it.
const RECOVERY_RESULT_MILLIS: u64 = 10_000;
/// Linux `MAX_ARG_STRLEN` is 128 KiB per argument; keep headroom below it.
const RECOVERY_ARGUMENT_BYTES: usize = 120 * 1024;
/// `bkpr session-record` exit code for a session without a claimed record.
const RECOVERY_UNCLAIMED_EXIT: i32 = 3;
/// `bkpr session-record` exit code for an update older than the record from this same
/// owner (its model was recreated); stdout carries the record's current sequence.
const RECOVERY_STALE_EXIT: i32 = 4;

/// The recoverable topology plus each included pane's removal key. Keys are terminal IDs
/// with their resumable agent association; they never enter the serialized record.
#[derive(Debug)]
struct RecoveryView {
    workspaces: Vec<RecoveryWorkspace>,
    keys: BTreeMap<u32, Option<String>>,
}

#[derive(Debug, PartialEq, Eq)]
enum RecoveryDecision {
    Send(RecoveryUpdate),
    WaitUntil(u64),
    Unchanged,
}

/// First retry delay after a failed, refused or lost update; doubles up to the cap.
const RECOVERY_RETRY_MILLIS: u64 = 1_000;
const RECOVERY_RETRY_MAX_MILLIS: u64 = 30_000;
/// Longest a restored session's first update waits for its expected panes.
const RESTORE_BASELINE_MILLIS: u64 = 10_000;

/// Restore-time sticky names from the startup owner configuration. Creation data, not
/// identity; persisted in the shared model so per-client replicas apply each name once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RestoreNames {
    /// Unclaimed sticky names by restore-layout leaf key.
    names: BTreeMap<String, String>,
    /// Leaf keys already claimed by their pane; later claims are ignored.
    consumed: BTreeSet<String>,
    /// Claimed names whose terminal has not reached the model yet.
    pending: BTreeMap<u32, String>,
    /// Unclaimed recorded associations by leaf key, kept for a retry if resuming fails.
    links: BTreeMap<String, RestoreLink>,
    /// Claimed associations by terminal. The recovery record keeps them while the pane has
    /// no detected agent; any detected agent or the pane closing ends them.
    retained: BTreeMap<u32, RestoreLink>,
}

#[derive(Deserialize)]
struct RestoreNameRequest {
    key: String,
    pane_id: String,
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

/// Restore-time owner seed: sticky names and retry links by leaf key, and the expected
/// restored terminal count.
#[derive(Debug, Default, PartialEq, Eq)]
struct RestoreSeed {
    names: BTreeMap<String, String>,
    links: BTreeMap<String, RestoreLink>,
    expected: Option<usize>,
}

/// Strip the restore-time owner keys (`restore_names`, a JSON `{leaf key: name}` object,
/// `restore_links`, a JSON `{leaf key: {harness, agent_id}}` object, and
/// `restore_expected_panes`, a decimal count) so validation, the storage identity and the
/// sidebar configurations derived from the owner's stay those of an ordinary session.
fn take_restore_seed(configuration: &mut BTreeMap<String, String>) -> Result<RestoreSeed, String> {
    // Remove all before parsing any, so a malformed value never leaves another behind.
    let names = configuration.remove("restore_names");
    let links = configuration.remove("restore_links");
    let expected = configuration.remove("restore_expected_panes");
    let names = match names {
        None => BTreeMap::new(),
        Some(json) => serde_json::from_str(&json)
            .map_err(|error| format!("Invalid restore_names owner configuration: {error}"))?,
    };
    let links = match links {
        None => BTreeMap::new(),
        Some(json) => serde_json::from_str(&json)
            .map_err(|error| format!("Invalid restore_links owner configuration: {error}"))?,
    };
    let expected = match expected {
        None => None,
        Some(count) => Some(count.parse::<usize>().map_err(|error| {
            format!("Invalid restore_expected_panes owner configuration: {error}")
        })?),
    };
    Ok(RestoreSeed {
        names,
        links,
        expected,
    })
}

/// The owner's application identity from its plugin configuration, plus the restore seed.
/// SDK routing, creation projections and restore-time keys are not identity, so neither the
/// storage identity nor any sidebar configuration derived from it ever carries them.
fn owner_identity(
    configuration: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Result<RestoreSeed, String>) {
    let mut configuration = configuration.clone();
    configuration.remove("caller_cwd");
    configuration.remove("mode_info_format");
    configuration.remove("sidebar_bootstrap");
    let seed = take_restore_seed(&mut configuration);
    (configuration, seed)
}

/// A new tab's sidebar configuration, before its creation bootstrap is added.
fn tab_sidebar_configuration(owner: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut configuration = owner.clone();
    configuration.remove("context_owner");
    configuration.insert("role".to_owned(), "sidebar".to_owned());
    configuration
}

/// Acknowledge CLI pipe input. The host shim is a no-op off WASM, so tests record it.
fn acknowledge_cli(source: &str) {
    #[cfg(test)]
    ACKNOWLEDGED_CLI.with(|acknowledged| acknowledged.borrow_mut().push(source.to_owned()));
    unblock_cli_pipe_input(source);
}

#[cfg(test)]
thread_local! {
    static ACKNOWLEDGED_CLI: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Owner-side write cadence for the session recovery record. Persisted in the shared
/// model so every per-client owner replica observes one sequence and one in-flight helper.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct RecoveryGate {
    sequence: u64,
    /// Last update the helper acknowledged as written (exit 0), with its removal keys.
    /// Unchanged detection and removal grace compare against this baseline only.
    acknowledged: Option<RecoveryUpdate>,
    acknowledged_keys: BTreeMap<u32, Option<String>>,
    /// Update handed to the helper and awaiting its result.
    pending: Option<RecoveryUpdate>,
    pending_keys: BTreeMap<u32, Option<String>>,
    /// Millis when the pending removal was first observed; kept until acknowledged so a
    /// retry does not restart the grace period.
    held_since: Option<u64>,
    /// Millis of the last handoff.
    last_sent: u64,
    /// Sequence awaiting its `RunCommandResult`.
    in_flight: Option<u64>,
    in_flight_since: u64,
    /// After a failure: resend (even unchanged content) no earlier than this.
    retry_at: Option<u64>,
    /// Current retry delay; zero after a success.
    backoff: u64,
    /// The session has no claimed record (it predates recovery); stop sending.
    disabled: bool,
    /// Restored sessions: terminal panes the restore layout spawns. The first update waits
    /// for them (or `RESTORE_BASELINE_MILLIS`) so a partial spawn cannot shrink the record.
    /// Cleared by the first acknowledged update.
    restore_expected: Option<usize>,
    /// Millis of the first offered topology while `restore_expected` is set.
    restore_since: Option<u64>,
}

impl RecoveryGate {
    /// Additions coalesce to one update per `RECOVERY_COALESCE_MILLIS`; removals (a pane
    /// key disappearing or losing its agent association) additionally wait out
    /// `RECOVERY_REMOVAL_GRACE_MILLIS`, so a shutdown never commits its own teardown.
    fn offer(&mut self, next: RecoveryView, now: u64, session: (&str, &str)) -> RecoveryDecision {
        if self.disabled {
            return RecoveryDecision::Unchanged;
        }
        if self.in_flight.is_some() {
            return RecoveryDecision::WaitUntil(
                self.in_flight_since.saturating_add(RECOVERY_RESULT_MILLIS),
            );
        }
        let restore_since = self
            .restore_expected
            .map(|_| *self.restore_since.get_or_insert(now));
        let (session_name, session_id) = session;
        let unchanged = self.retry_at.is_none()
            && self.acknowledged.as_ref().is_some_and(|acknowledged| {
                acknowledged.session_name == session_name
                    && acknowledged.session_id == session_id
                    && acknowledged.workspaces == next.workspaces
            });
        // An empty topology is never recoverable: it is startup before adoption or teardown.
        if unchanged || next.workspaces.is_empty() {
            self.held_since = None;
            return RecoveryDecision::Unchanged;
        }
        let removal = self
            .acknowledged_keys
            .iter()
            .any(|(id, agent)| match next.keys.get(id) {
                None => true,
                Some(current) => agent.is_some() && current.is_none(),
            });
        let mut due = if self.sequence > 0 {
            self.last_sent.saturating_add(RECOVERY_COALESCE_MILLIS)
        } else {
            now
        };
        if let Some(retry_at) = self.retry_at {
            due = due.max(retry_at);
        }
        if removal {
            let since = *self.held_since.get_or_insert(now);
            due = due.max(since.saturating_add(RECOVERY_REMOVAL_GRACE_MILLIS));
        } else {
            self.held_since = None;
        }
        if let (Some(expected), Some(since)) = (self.restore_expected, restore_since)
            && next.keys.len() < expected
        {
            due = due.max(since.saturating_add(RESTORE_BASELINE_MILLIS));
        }
        if now < due {
            return RecoveryDecision::WaitUntil(due);
        }
        self.sequence += 1;
        let update = RecoveryUpdate {
            session_name: session_name.to_owned(),
            session_id: session_id.to_owned(),
            sequence: self.sequence,
            workspaces: next.workspaces,
        };
        self.pending = Some(update.clone());
        self.pending_keys = next.keys;
        self.last_sent = now;
        self.in_flight = Some(self.sequence);
        self.in_flight_since = now;
        RecoveryDecision::Send(update)
    }

    /// Settle the helper result for `sequence`; false for a stale or foreign result.
    fn finish(&mut self, sequence: u64, code: Option<i32>, stdout: &[u8], now: u64) -> bool {
        if self.in_flight != Some(sequence) {
            return false;
        }
        self.in_flight = None;
        match code {
            Some(0) => {
                self.acknowledged = self.pending.take();
                self.acknowledged_keys = std::mem::take(&mut self.pending_keys);
                self.restore_expected = None;
                self.restore_since = None;
                self.held_since = None;
                self.retry_at = None;
                self.backoff = 0;
            }
            Some(RECOVERY_UNCLAIMED_EXIT) => {
                self.disabled = true;
                self.pending = None;
                self.pending_keys.clear();
            }
            Some(RECOVERY_STALE_EXIT) => match std::str::from_utf8(stdout)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
            {
                // Continue past the record and resend without backoff, even if unchanged.
                Some(current) => {
                    self.sequence = self.sequence.max(current);
                    self.pending = None;
                    self.pending_keys.clear();
                    self.retry_at = Some(now);
                }
                None => self.fail(now),
            },
            _ => self.fail(now),
        }
        true
    }

    /// Give up on an in-flight helper that has not reported within `RECOVERY_RESULT_MILLIS`.
    fn expire(&mut self, now: u64) -> bool {
        let stale = self.in_flight.is_some()
            && now.saturating_sub(self.in_flight_since) >= RECOVERY_RESULT_MILLIS;
        if stale {
            self.in_flight = None;
            self.fail(now);
        }
        stale
    }

    /// The sent update could not be handed to the helper at all (e.g. too large).
    fn refuse(&mut self, now: u64) {
        self.in_flight = None;
        self.fail(now);
    }

    /// Drop the unwritten update and schedule a resend against the acknowledged baseline.
    fn fail(&mut self, now: u64) {
        self.pending = None;
        self.pending_keys.clear();
        self.backoff = if self.backoff == 0 {
            RECOVERY_RETRY_MILLIS
        } else {
            self.backoff
                .saturating_mul(2)
                .min(RECOVERY_RETRY_MAX_MILLIS)
        };
        self.retry_at = Some(now.saturating_add(self.backoff));
    }
}

/// Sidebar error for a settled helper result. Unclaimed sessions disable recovery silently.
fn recovery_failure(code: Option<i32>, stderr: &[u8]) -> Option<String> {
    match code {
        Some(0 | RECOVERY_UNCLAIMED_EXIT | RECOVERY_STALE_EXIT) => None,
        _ => Some(format!(
            "Session recovery update failed: {}",
            crate::ui::clean(&String::from_utf8_lossy(&stderr[..stderr.len().min(4_096)]))
        )),
    }
}

/// The `--json` argument for `session-record`, refused before it could exceed argv limits.
fn recovery_argument(update: &RecoveryUpdate) -> Result<String, String> {
    let json = serde_json::to_string(update).expect("serializing recovery update");
    if json.len() > RECOVERY_ARGUMENT_BYTES {
        return Err(format!(
            "Session recovery update too large ({} bytes; limit {RECOVERY_ARGUMENT_BYTES}); the recovery record is no longer updated",
            json.len()
        ));
    }
    Ok(json)
}

/// Tiled terminal rectangles per native tab ID. Floating and plugin panes (including the
/// sidebar) are not part of the recoverable split tree.
///
/// The runtime folds a stack into one visible member and parks the others as suppressed
/// panes resized to its exact rectangle (in every tab, not only the active one), so a
/// suppressed terminal sharing a visible tiled terminal's rectangle is a hidden stack
/// member. Other suppressed panes (no visible counterpart) stay excluded.
fn recovery_geometry(
    tabs: &[TabInfo],
    manifest: &PaneManifest,
) -> BTreeMap<usize, Vec<GeometryPane>> {
    let geometry = |pane: &PaneInfo| GeometryPane {
        id: pane.id,
        x: pane.pane_x,
        y: pane.pane_y,
        columns: pane.pane_columns,
        rows: pane.pane_rows,
    };
    let rectangle = |pane: &GeometryPane| (pane.x, pane.y, pane.columns, pane.rows);
    tabs.iter()
        .filter_map(|tab| {
            let panes = manifest.panes.get(&tab.position)?;
            let terminals = || {
                panes
                    .iter()
                    .filter(|pane| !pane.is_plugin && !pane.is_floating)
            };
            let mut tiled: Vec<_> = terminals()
                .filter(|pane| !pane.is_suppressed)
                .map(geometry)
                .collect();
            let visible: BTreeSet<_> = tiled.iter().map(rectangle).collect();
            tiled.extend(
                terminals()
                    .filter(|pane| pane.is_suppressed)
                    .map(geometry)
                    .filter(|pane| visible.contains(&rectangle(pane))),
            );
            Some((tab.tab_id, tiled))
        })
        .collect()
}

/// Recovery stores only harnesses it can resume.
fn recovery_harness(harness: &str) -> Option<&'static str> {
    match harness {
        "claude" | "claude-code" => Some("claude"),
        "omp" => Some("omp"),
        "codex" => Some("codex"),
        _ => None,
    }
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

    fn harness_records(&self, has_process_harness: bool) -> impl Iterator<Item = &ProgramStatus> {
        self.records.iter().filter(move |record| {
            record
                .app
                .as_deref()
                .map_or(has_process_harness, |app| harness_name(app).is_some())
        })
    }

    fn harness_record(&self, has_process_harness: bool) -> Option<&ProgramStatus> {
        self.harness_records(has_process_harness)
            .max_by_key(|record| record.status().priority())
    }

    fn harness_attention(&self, has_process_harness: bool) -> Option<Attention> {
        Attention::aggregate(
            self.harness_records(has_process_harness)
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

    fn same_display(&self, other: &Self, renamed: bool, has_process_harness: bool) -> bool {
        let (Some(previous), Some(current)) = (self.dominant(), other.dominant()) else {
            return false;
        };
        let previous_harness = self.harness_record(has_process_harness);
        let current_harness = other.harness_record(has_process_harness);
        previous.state == current.state
            && previous.kind == current.kind
            && previous.progress == current.progress
            && previous.app == current.app
            && previous.msg == current.msg
            && (renamed || self.title() == other.title())
            && previous_harness.map(ProgramStatus::status)
                == current_harness.map(ProgramStatus::status)
            && previous_harness.and_then(|record| record.app.as_deref())
                == current_harness.and_then(|record| record.app.as_deref())
            && self.harness_attention(has_process_harness)
                == other.harness_attention(has_process_harness)
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
    let owner = role == "native-owner";
    configuration.len() == (if owner { 8 } else { 7 })
        && matches!(role, "native-owner" | "sidebar")
        && configuration.get("role").map(String::as_str) == Some(role)
        && configuration.get("session").is_some_and(|s| !s.is_empty())
        && configuration
            .get("session_id")
            .is_some_and(|s| !s.is_empty())
        && (if owner {
            configuration
                .get("context_owner")
                .is_some_and(|s| !s.is_empty())
        } else {
            !configuration.contains_key("context_owner")
        })
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
    return_mode: Option<InputMode>,
    view: Option<&SidebarView>,
) -> Result<(), serde_json::Error> {
    let payload = serde_json::to_string(&NativeRequestRef {
        client,
        client_id,
        request,
        return_mode,
        view,
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
    let session_id = configuration.get("session_id").ok_or_else(|| {
        std::io::Error::other("Cannot restart native owner without session identity")
    })?;
    post_message_to_plugin(PluginMessage {
        name: "bkpr_context_owner_restart".to_owned(),
        payload: serde_json::json!({
            "session_id": session_id,
            "closed_owner": closed_id,
        })
        .to_string(),
        ..PluginMessage::default()
    });
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
    #[serde(default)]
    host_context: Option<HostContext>,
    #[serde(default)]
    context_sequence: u64,
    #[serde(default)]
    context_generation: u64,
    #[serde(default)]
    context_published: Option<ContextProjection>,
    #[serde(default)]
    context_publisher: Option<u32>,
    #[serde(default)]
    context_error: Option<String>,
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
    /// Canonical repository assignments, released only after their last pane leaves.
    // LEGACY_COMPAT(next-major): seed assignments when recovering pre-allocation models.
    #[serde(default)]
    project_colors: BTreeMap<String, usize>,
    // Preserve identity reservations through asynchronous metadata invalidation/CWD changes.
    #[serde(default)]
    project_color_repositories: BTreeMap<u32, String>,
    project_error: Option<String>,
    frontends: BTreeSet<u32>,
    receipts: BTreeMap<String, Receipt>,
    #[serde(default)]
    recovery: RecoveryGate,
    #[serde(default)]
    recovery_error: Option<String>,
    #[serde(default)]
    restore: RestoreNames,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Project {
    name: String,
    repository: bool,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    branch_changes: Option<bkpr::BranchChanges>,
    #[serde(default)]
    repository_id: Option<String>,
    #[serde(default)]
    ci: Option<CiSnapshot>,
}

impl Project {
    fn ci_identity_current(&self, ci: &CiSnapshot) -> bool {
        !ci.local_head_sha.is_empty()
            && self.head_sha.as_deref() == Some(ci.local_head_sha.as_str())
    }

    fn identity(&self, cwd: &str, colors: &BTreeMap<String, usize>) -> ProjectIdentity {
        ProjectIdentity {
            id: self
                .repository_id
                .clone()
                .unwrap_or_else(|| format!("directory:{cwd}")),
            name: self.name.clone(),
            repository: self.repository && self.repository_id.is_some(),
            color_slot: self
                .repository_id
                .as_ref()
                .filter(|_| self.repository)
                .and_then(|id| colors.get(id).copied()),
        }
    }
}

impl Model {
    fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        match serde_json::from_slice::<Self>(bytes) {
            Ok(mut model) => {
                model.invalidate_unidentified_ci();
                if model.sync_project_colors() {
                    model.revision += 1;
                }
                Ok(model)
            }
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
                model.invalidate_unidentified_ci();
                if model.sync_project_colors() {
                    model.revision += 1;
                }
                Ok(model)
            }
        }
    }

    fn invalidate_unidentified_ci(&mut self) {
        for (cwd, project) in &mut self.projects {
            if project
                .ci
                .as_ref()
                .is_some_and(|ci| !project.ci_identity_current(ci))
            {
                project.ci = None;
                *self.project_generations.entry(cwd.clone()).or_default() += 1;
                self.project_watches.remove(cwd);
                self.pending_projects.remove(cwd);
                self.revision += 1;
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
            host_context: None,
            context_sequence: 0,
            context_generation: 0,
            context_published: None,
            context_publisher: None,
            context_error: None,
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
            project_colors: BTreeMap::new(),
            project_color_repositories: BTreeMap::new(),
            frontends: BTreeSet::new(),
            receipts: BTreeMap::new(),
            project_error: None,
            recovery: RecoveryGate::default(),
            recovery_error: None,
            restore: RestoreNames::default(),
        }
    }

    /// Claim `key`'s sticky name for terminal `pane`; false for unknown or consumed keys.
    fn restore_name(&mut self, key: &str, pane: u32) -> bool {
        let Some(name) = self.restore.names.remove(key) else {
            return false;
        };
        self.restore.consumed.insert(key.to_owned());
        self.restore.pending.insert(pane, name);
        true
    }

    /// Claim `key`'s recorded association for terminal `pane`; false for unknown or consumed
    /// keys and for a claim that does not match the seeded association exactly.
    fn restore_link(&mut self, key: &str, pane: u32, link: &RestoreLink) -> bool {
        if self.restore.links.get(key) != Some(link) {
            return false;
        }
        self.restore.links.remove(key);
        self.restore.consumed.insert(key.to_owned());
        self.restore.retained.insert(pane, link.clone());
        self.prune_retained_links();
        true
    }

    /// End retained associations whose pane closed or now has a detected agent.
    fn prune_retained_links(&mut self) -> bool {
        let before = self.restore.retained.len();
        let closed = &self.closed_panes;
        let agents = &self.agents;
        self.restore.retained.retain(|id, _| {
            !closed.contains(id)
                && agents
                    .get(id)
                    .is_none_or(|agent| harness_name(&agent.harness).is_none())
        });
        self.restore.retained.len() != before
    }

    /// Apply claimed names to terminals now in the model; returns them for native renaming.
    /// Claims for terminals that closed before arriving are dropped.
    fn apply_restore_names(&mut self) -> Vec<(u32, String)> {
        let closed = &self.closed_panes;
        self.restore.pending.retain(|id, _| !closed.contains(id));
        let ready: Vec<_> = self
            .restore
            .pending
            .keys()
            .filter(|id| self.panes.contains_key(id))
            .copied()
            .collect();
        let mut applied = Vec::new();
        for id in ready {
            let name = self
                .restore
                .pending
                .remove(&id)
                .expect("pending restore name");
            let pane = self.panes.get_mut(&id).expect("restored terminal");
            pane.name = name.clone();
            pane.renamed = true;
            applied.push((id, name));
        }
        applied
    }

    fn workspace(&mut self, name: String) -> String {
        let id = loop {
            self.next_workspace += 1;
            let id = format!(
                "{}:workspace:bkpr:{}",
                self.configuration["session_id"], self.next_workspace
            );
            if !self.workspaces.contains_key(&id) {
                break id;
            }
        };
        self.workspaces.insert(id.clone(), NativeWorkspace { name });
        id
    }

    fn adopt_context(&mut self, context: HostContext) -> Result<bool, &'static str> {
        if context.version != 1
            || self.configuration.get("session_id") != Some(&context.session_id)
            || context.session_name.is_empty()
        {
            return Err("Native context has an invalid session identity");
        }
        let mut tabs = BTreeMap::new();
        let mut names = BTreeMap::new();
        for tab in &context.tabs {
            if tab.workspace_id.is_empty()
                || tab.workspace_name.is_empty()
                || tabs.insert(tab.tab_id, tab.workspace_id.clone()).is_some()
                || names
                    .insert(tab.workspace_id.clone(), tab.workspace_name.clone())
                    .is_some_and(|name| name != tab.workspace_name)
            {
                return Err("Native context has inconsistent workspace bindings");
            }
        }
        let mut panes = BTreeMap::new();
        for pane in &context.panes {
            if tabs.get(&pane.tab_id) != Some(&pane.workspace_id)
                || names.get(&pane.workspace_id) != Some(&pane.workspace_name)
            {
                return Err("Native context has inconsistent terminal bindings");
            }
            if pane
                .pane_id
                .strip_prefix("plugin_")
                .is_some_and(|id| id.parse::<u32>().is_ok())
            {
                continue;
            }
            let Some(id) = terminal_id(&pane.pane_id) else {
                return Err("Native context has an invalid terminal identity");
            };
            if panes.insert(id, pane).is_some() {
                return Err("Native context has inconsistent terminal bindings");
            }
        }
        self.closed_tabs.retain(|tab| {
            tabs.get(tab).is_some_and(|workspace| {
                self.host_context
                    .as_ref()
                    .and_then(|previous| previous.tabs.iter().find(|old| old.tab_id == *tab))
                    .is_none_or(|old| old.workspace_id == *workspace)
            })
        });
        let mut changed = self.host_context.as_ref() != Some(&context);
        let removed: Vec<_> = self
            .panes
            .keys()
            .filter(|id| !panes.contains_key(*id))
            .copied()
            .collect();
        for id in removed {
            changed |= self.close_pane(id);
        }
        for (id, binding) in &panes {
            if let Some(pane) = self.panes.get_mut(id)
                && (pane.workspace != binding.workspace_id || pane.tab != binding.tab_id)
            {
                pane.workspace = binding.workspace_id.clone();
                pane.tab = binding.tab_id;
                self.agent_generation += 1;
                self.agents_dirty = true;
                changed = true;
            }
        }
        for (tab, workspace) in &tabs {
            if !self.closed_tabs.contains(tab) {
                self.tabs.insert(*tab, workspace.clone());
            }
        }
        self.tabs.retain(|tab, _| {
            !self.closed_tabs.contains(tab)
                && (tabs.contains_key(tab) || self.pending_tabs.contains(tab))
        });
        for (id, name) in names {
            self.workspaces.insert(id, NativeWorkspace { name });
        }
        self.workspaces
            .retain(|id, _| self.tabs.values().any(|workspace| workspace == id));
        self.host_context = Some(context);
        Ok(changed)
    }

    fn context_projection(&self) -> Option<ContextProjection> {
        let context = self.host_context.as_ref()?;
        let workspaces: BTreeMap<_, _> = context
            .tabs
            .iter()
            .filter(|tab| !self.closed_tabs.contains(&tab.tab_id))
            .filter(|tab| {
                !self.pending_tabs.contains(&tab.tab_id)
                    || self.panes.values().any(|pane| pane.tab == tab.tab_id)
            })
            .map(|tab| (tab.workspace_id.clone(), tab.workspace_name.clone()))
            .collect();
        Some(ContextProjection {
            session_name: context.session_name.clone(),
            workspaces: workspaces
                .into_iter()
                .map(|(id, name)| ContextWorkspace { id, name })
                .collect(),
            panes: self
                .panes
                .iter()
                .map(|(id, pane)| ContextPane {
                    pane_id: format!("terminal_{id}"),
                    workspace_id: pane.workspace.clone(),
                    pane_name: self.pane_name(*id, pane),
                    agent_id: self
                        .pane_agent(*id)
                        .and_then(|agent| agent.agent_id.clone()),
                    status: self.pane_status(*id),
                })
                .collect(),
        })
    }

    /// Published harness status follows the sidebar's harness gating: only an
    /// OSC 7501 record from a recognized harness, never generic program status.
    fn pane_status(&self, id: u32) -> Option<PaneStatus> {
        let has_process_harness = self.pane_agent(id).is_some();
        let program = self.program_status.get(&id)?;
        let record = program.harness_record(has_process_harness)?;
        Some(PaneStatus {
            state: record.status(),
            attention: program.harness_attention(has_process_harness),
            message: record
                .msg
                .as_deref()
                .map(display_text)
                .filter(|message| !message.is_empty()),
        })
    }

    fn pane_agent(&self, id: u32) -> Option<&AgentExecution> {
        self.agents
            .get(&id)
            .filter(|agent| harness_name(&agent.harness).is_some())
    }

    fn pane_name(&self, id: u32, pane: &NativePane) -> String {
        if pane.renamed {
            return pane.name.clone();
        }
        let agent = self.pane_agent(id);
        let program = self.program_status.get(&id);
        let harness = program
            .and_then(|program| program.harness_record(agent.is_some()))
            .and_then(|record| record.app.as_deref())
            .or_else(|| agent.map(|agent| agent.harness.as_str()));
        program
            .and_then(NativeProgram::label)
            .or_else(|| {
                pane.has_native_title
                    .then(|| native_title(&pane.name, harness).to_owned())
            })
            .or_else(|| {
                program
                    .and_then(NativeProgram::dominant)
                    .and_then(|record| record.app.as_deref())
                    .map(|app| harness_name(app).unwrap_or(app).to_owned())
            })
            .or_else(|| agent.and_then(|agent| harness_name(&agent.harness).map(str::to_owned)))
            .unwrap_or_else(|| pane.name.clone())
    }

    /// Recoverable topology: workspaces in key order, their tabs by native position, each
    /// tab's tiled terminals (`geometry`, keyed by tab ID) as a split tree.
    fn recovery_workspaces(
        &self,
        geometry: &BTreeMap<usize, Vec<GeometryPane>>,
        tabs: &[TabInfo],
        focused: Option<u32>,
    ) -> RecoveryView {
        let mut ordered: Vec<_> = tabs.iter().collect();
        ordered.sort_by_key(|tab| tab.position);
        let mut keys = BTreeMap::new();
        let mut workspaces = Vec::new();
        for (workspace_id, workspace) in &self.workspaces {
            let mut recovered = Vec::new();
            for tab in &ordered {
                if self.tabs.get(&tab.tab_id) != Some(workspace_id) {
                    continue;
                }
                let panes: Vec<_> = geometry
                    .get(&tab.tab_id)
                    .into_iter()
                    .flatten()
                    .filter(|pane| {
                        self.panes
                            .get(&pane.id)
                            .is_some_and(|native| native.tab == tab.tab_id)
                    })
                    .copied()
                    .collect();
                let Some(tree) = split_tree(&panes) else {
                    continue;
                };
                let layout = self.recovery_layout(tree, focused, &mut keys);
                recovered.push(RecoveryTab {
                    name: tab.name.clone(),
                    focused: layout.leaves().iter().any(|pane| pane.focused),
                    layout,
                });
            }
            if !recovered.is_empty() {
                workspaces.push(RecoveryWorkspace {
                    name: workspace.name.clone(),
                    tabs: recovered,
                });
            }
        }
        RecoveryView { workspaces, keys }
    }

    fn recovery_layout(
        &self,
        node: SplitNode<u32>,
        focused: Option<u32>,
        keys: &mut BTreeMap<u32, Option<String>>,
    ) -> SplitNode<RecoveryPane> {
        let mut leaf = |id: u32| {
            let pane = &self.panes[&id];
            let (harness, agent_id) = match self.pane_agent(id) {
                Some(agent) => match recovery_harness(&agent.harness) {
                    Some(harness) => (Some(harness), agent.agent_id.clone()),
                    None => (None, None),
                },
                // No detected agent: a restored pane keeps an association whose resume was
                // refused or failed, so a later resume can retry it.
                None => self
                    .restore
                    .retained
                    .get(&id)
                    .and_then(|link| {
                        Some((recovery_harness(&link.harness)?, link.agent_id.clone()))
                    })
                    .map_or((None, None), |(harness, agent_id)| {
                        (Some(harness), Some(agent_id))
                    }),
            };
            keys.insert(id, agent_id.clone());
            RecoveryPane {
                cwd: pane.cwd.clone(),
                name: self.pane_name(id, pane),
                renamed: pane.renamed,
                harness: harness.map(str::to_owned),
                agent_id,
                focused: focused == Some(id),
            }
        };
        match node {
            SplitNode::Pane { pane } => SplitNode::Pane { pane: leaf(pane) },
            SplitNode::Stack { panes } => SplitNode::Stack {
                panes: panes.into_iter().map(leaf).collect(),
            },
            SplitNode::Split {
                direction,
                children,
            } => SplitNode::Split {
                direction,
                children: children
                    .into_iter()
                    .map(|child| self.recovery_layout(child, focused, keys))
                    .collect(),
            },
        }
    }

    fn reserve_context(
        &mut self,
        server: i32,
        owner: u32,
        reloaded: bool,
    ) -> Option<ContextPublication> {
        let projection = self.context_projection()?;
        if !reloaded
            && self.context_publisher == Some(owner)
            && self.context_published.as_ref() == Some(&projection)
        {
            return None;
        }
        self.context_sequence = self.context_sequence.checked_add(1)?;
        self.context_publisher = Some(owner);
        self.context_published = Some(projection.clone());
        Some(ContextPublication {
            session_id: self.configuration["session_id"].clone(),
            session_name: projection.session_name,
            server,
            owner,
            sequence: self.context_sequence,
            token: self.configuration["context_owner"].clone(),
            workspaces: projection.workspaces,
            panes: projection.panes,
        })
    }

    fn snapshot(&self, error: Option<String>, client_id: u16) -> Snapshot {
        let panes: Vec<_> = self
            .panes
            .iter()
            .map(|(id, pane)| {
                let agent = self.pane_agent(*id);
                let program = self.program_status.get(id);
                let project = pane.cwd.as_deref().and_then(|cwd| self.projects.get(cwd));
                let record = program.and_then(NativeProgram::dominant);
                let harness_record =
                    program.and_then(|program| program.harness_record(agent.is_some()));
                let harness = harness_record
                    .and_then(|record| record.app.as_deref())
                    .or_else(|| agent.map(|agent| agent.harness.as_str()));
                Pane {
                    id: format!("terminal_{id}"),
                    terminal_id: format!("terminal_{id}"),
                    workspace_id: pane.workspace.clone(),
                    tab_id: format!("tab_{}", pane.tab),
                    name: self.pane_name(*id, pane),
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
                    harness: harness.map(str::to_owned),
                    agent_id: agent.and_then(|agent| agent.agent_id.clone()),
                    // Generic raw status is retained only on non-harness panes; presentation
                    // and workspace summaries must gate program status on harness identity.
                    status: if let Some(record) = harness_record {
                        record.status()
                    } else if harness.is_some() {
                        Status::Unknown
                    } else if let Some(record) = record {
                        record.status()
                    } else if pane.shell {
                        Status::Idle
                    } else {
                        Status::Unknown
                    },
                    attention: program
                        .and_then(|program| program.harness_attention(agent.is_some())),
                    cwd: pane.cwd.clone(),
                    branch: project.and_then(|project| project.branch.clone()),
                    branch_changes: project.and_then(|project| project.branch_changes.clone()),
                    project: project
                        .filter(|project| project.repository && project.repository_id.is_some())
                        .map(|project| {
                            project.identity(
                                pane.cwd.as_deref().unwrap_or_default(),
                                &self.project_colors,
                            )
                        }),
                    ci: project.and_then(|project| project.ci.clone()),
                }
            })
            .collect();
        let workspaces = self
            .workspaces
            .iter()
            // Tab bindings are persisted before Zellij delivers the new terminal's topology.
            // A pending-only workspace is not public topology yet, including during replay
            // of an older coherent report or an unrelated metadata publication.
            .filter(|(id, _)| {
                panes.iter().any(|pane| &pane.workspace_id == *id)
                    || self.tabs.iter().any(|(tab, workspace)| {
                        workspace == *id && !self.pending_tabs.contains(tab)
                    })
            })
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
                                project.identity(cwd, &self.project_colors),
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
                        .filter(|pane| pane.harness.is_some())
                        .map(|pane| pane.status)
                        .max_by_key(|status| status.priority())
                        .unwrap_or(Status::Unknown),
                    attention: Attention::aggregate(
                        members()
                            .filter(|pane| pane.harness.is_some() && pane.status == Status::Blocked)
                            .map(|pane| pane.attention),
                    ),
                }
            })
            .collect();
        Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id: self.instance.clone(),
            revision: self.revision,
            session: self.host_context.as_ref().map_or_else(
                || self.configuration["session"].clone(),
                |context| context.session_name.clone(),
            ),
            connected: error.is_none(),
            error: error
                .or_else(|| self.project_error.clone())
                .or_else(|| self.agent_error.clone())
                .or_else(|| self.context_error.clone())
                .or_else(|| self.recovery_error.clone()),
            focused_pane_id: self
                .focus
                .get(&client_id)
                .map(|id| format!("terminal_{id}")),
            workspaces,
            panes,
        }
    }

    fn reconcile_topology(&mut self, tabs: &[TabInfo], manifest: &PaneManifest) -> bool {
        let Some(context) = &self.host_context else {
            return false;
        };
        let bindings: BTreeMap<_, _> = context
            .tabs
            .iter()
            .map(|tab| (tab.tab_id, tab.workspace_id.clone()))
            .collect();
        let pane_bindings: BTreeMap<_, _> = context
            .panes
            .iter()
            .filter_map(|pane| terminal_id(&pane.pane_id).map(|id| (id, pane.tab_id)))
            .collect();
        let mut changed = false;
        let mut agent_topology_changed = false;
        // Zellij terminal IDs are monotonic for the server lifetime. Keep close tombstones
        // across topology callbacks so a delayed notification cannot resurrect a terminal.
        let mut alive: BTreeSet<_> = context
            .panes
            .iter()
            .filter_map(|pane| terminal_id(&pane.pane_id))
            .filter(|id| self.panes.contains_key(id) && !self.closed_panes.contains(id))
            .collect();
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
                .filter(|p| {
                    !p.is_plugin
                        && !self.closed_panes.contains(&p.id)
                        && pane_bindings.get(&p.id) == Some(&tab.tab_id)
                })
                .collect();
            terminals.sort_by_key(|p| p.id);
            let Some(workspace) = bindings.get(&tab.tab_id).cloned() else {
                // The topology stream may precede its canonical host binding.
                alive.extend(
                    self.panes
                        .iter()
                        .filter(|(_, pane)| pane.tab == tab.tab_id)
                        .map(|(id, _)| *id),
                );
                continue;
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
        changed |= self.prune_retained_links();
        let previous_count = self.panes.len();
        self.panes.retain(|id, _| alive.contains(id));
        changed |= self.panes.len() != previous_count;
        agent_topology_changed |= self.panes.len() != previous_count;
        self.agents.retain(|id, _| alive.contains(id));
        self.program_status
            .retain(|id, _| !self.closed_panes.contains(id));
        self.agent_roots.retain(|id, _| alive.contains(id));
        // Preserve new bindings until their terminal arrives, not merely the tab/plugin.
        let known_tabs: BTreeSet<_> = bindings
            .keys()
            .filter(|tab| !self.closed_tabs.contains(*tab))
            .copied()
            .collect();
        let previous_count = self.tabs.len();
        self.pending_tabs
            .retain(|id| !self.panes.values().any(|pane| pane.tab == *id));
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
        self.prune_retained_links();
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

    fn sync_project_colors(&mut self) -> bool {
        self.project_color_repositories
            .retain(|pane, _| self.panes.contains_key(pane));
        for (id, pane) in &self.panes {
            let Some(project) = pane.cwd.as_ref().and_then(|cwd| self.projects.get(cwd)) else {
                // A branch refresh or worktree CWD change must not free a live reservation
                // before the asynchronous canonical-identity result arrives.
                continue;
            };
            if let Some(repository) = project
                .repository_id
                .as_ref()
                .filter(|_| project.repository)
            {
                if self.project_color_repositories.get(id) != Some(repository) {
                    self.project_color_repositories
                        .insert(*id, repository.clone());
                }
            } else {
                self.project_color_repositories.remove(id);
            }
        }
        let live: BTreeSet<_> = self.project_color_repositories.values().collect();
        let previous_count = self.project_colors.len();
        self.project_colors.retain(|id, _| live.contains(id));
        let mut changed = previous_count != self.project_colors.len();
        let mut occupied: BTreeSet<_> = self.project_colors.values().copied().collect();
        let mut available = 0;
        for repository in live {
            if self.project_colors.contains_key(repository) {
                continue;
            }
            while occupied.contains(&available) {
                available += 1;
            }
            self.project_colors.insert(repository.clone(), available);
            changed = true;
            occupied.insert(available);
        }
        changed
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
        self.sync_project_colors();
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
                        !target.head_sha.is_empty()
                            && project.ci_identity_current(ci)
                            && ci.repository == target.repository
                            && ci.head_sha == target.head_sha
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
        if let Some(mut project) = update.project {
            if project
                .ci
                .as_ref()
                .is_some_and(|ci| !project.ci_identity_current(ci))
            {
                project.ci = None;
            }
            let changed =
                self.projects.get(&update.cwd) != Some(&project) || self.project_error.is_some();
            self.projects.insert(update.cwd.clone(), project);
            self.failed_projects.remove(&update.cwd);
            self.project_error = None;
            self.sync_project_colors();
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
        // Zellij reports a restored pane's spawn command whenever no foreground child runs,
        // even after the helper exec'd into the shell: keep it unknown and re-query.
        let command = if is_restore_helper(&command) {
            pane.metadata_initialized = false;
            Vec::new()
        } else {
            command
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
            let has_process_harness = self
                .agents
                .get(&pane_id)
                .is_some_and(|agent| harness_name(&agent.harness).is_some());
            previous.is_none_or(|previous| {
                !previous.same_display(&program, pane.renamed, has_process_harness)
            })
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
            let previous = self
                .agents
                .get(id)
                .filter(|agent| harness_name(&agent.harness).is_some());
            let current = agents
                .get(id)
                .filter(|agent| harness_name(&agent.harness).is_some());
            changed |= previous.map(|agent| &agent.harness) != current.map(|agent| &agent.harness);
            changed |= previous.and_then(|agent| agent.agent_id.as_ref())
                != current.and_then(|agent| agent.agent_id.as_ref());
        }
        for (id, agent) in &agents {
            if let Some(cwd) = &agent.cwd {
                changed |= self.observe_cwd(*id, cwd.clone());
            }
        }
        self.agents = agents;
        changed |= self.prune_retained_links();
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
    // Host guide state belongs to this client replica, not the persisted canonical model.
    guide_mode: Option<GuideMode>,
    keymap_cache: Option<KeymapCacheRef>,
    keymap_payload: String,
    keymap_generation: u64,
    context_ready: bool,
    context_reload: bool,
    // Deadline of this replica's armed recovery timer; avoids re-arming on every callback.
    recovery_timer: Option<u64>,
    control_receipts: std::collections::VecDeque<(String, String)>,
}

impl Owner {
    pub fn load(&mut self, configuration: &BTreeMap<String, String>) {
        let (configuration, restore_seed) = owner_identity(configuration);
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
        self.guide_mode = None;
        self.keymap_cache = None;
        self.keymap_payload.clear();
        self.keymap_generation = 0;
        self.context_ready = false;
        self.context_reload = true;
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
            EventType::ModeUpdate,
            EventType::InitialKeybinds,
            EventType::CustomMessage,
        ]);
        if !configuration_valid(&configuration, "native-owner") {
            self.error = Some(format!(
                "Invalid native owner configuration: keys {:?}",
                configuration.keys().collect::<Vec<_>>()
            ));
            return;
        }
        let seed = match restore_seed {
            Ok(seed) => seed,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let pid = ids.zellij_pid;
        let identity = serde_json::to_vec(&configuration).expect("serializing owner configuration");
        let hash = identity.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        self.storage = PathBuf::from(format!("/cache/bkpr-native-{pid}-{hash:016x}.json"));
        match std::fs::read(&self.storage) {
            Ok(bytes) => match Model::decode(&bytes) {
                Ok(model) if model.configuration == configuration => {
                    self.model = Some(model);
                    self.storage_bytes = bytes;
                }
                _ => self.error = Some("Native metadata cache is corrupt or has a conflicting identity; refusing to reset labels".to_owned()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut model = Model::new(configuration.clone(), format!("native:{pid}:{hash:016x}"));
                // Only a new model is seeded: an existing one already holds the claims.
                model.restore.names = seed.names;
                model.restore.links = seed.links;
                model.recovery.restore_expected = seed.expected;
                self.model = Some(model);
            }
            Err(error) => self.error = Some(format!("Cannot recover native metadata: {error}")),
        }
        // Context publication is a background service, not a consequence of sidebar registration.
        if self.error.is_none() {
            self.permission_requested = true;
            request_permission(super::PERMISSIONS);
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

    fn cache_keymap(&mut self, payload: String) {
        // Compare all bytes, not a revision, size, or digest, before avoiding a decode/write.
        if self.keymap_cache.is_some() && self.keymap_payload == payload {
            return;
        }
        if serde_json::from_str::<KeybindsVec>(&payload).is_err() {
            return;
        }
        let Some(model) = self.model.as_ref() else {
            return;
        };
        self.keymap_generation += 1;
        let reference = KeymapCacheRef {
            instance_id: model.instance.clone(),
            source: format!("{}:{}", self.server, self.selection_epoch),
            client_id: self.client_id,
            generation: self.keymap_generation,
            content_hash: keymap_hash(&payload),
        };
        let Some(path) = reference.path() else {
            return;
        };
        let cache = KeymapCache { reference, payload };
        let result = serde_json::to_vec(&cache)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                let temporary = path.with_extension("pending");
                std::fs::write(&temporary, bytes)?;
                std::fs::rename(temporary, &path)
            });
        match result {
            Ok(()) => {
                self.keymap_cache = Some(cache.reference);
                self.keymap_payload = cache.payload;
            }
            Err(error) => {
                // Do not permit a later creation to reuse a stale guide after a remap.
                self.keymap_cache = None;
                self.error = Some(format!("Cannot retain native keymap: {error}"));
            }
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

    fn snapshot_error(&self) -> Option<String> {
        self.error.clone().or_else(|| {
            (!self.context_ready).then(|| {
                "Native workspace context is unavailable until the host reports its bindings"
                    .to_owned()
            })
        })
    }

    fn state(&self, client_id: u16) -> Option<NativeState> {
        let own_client = client_id == self.client_id;
        Some(NativeState {
            client_id,
            snapshot: self
                .model
                .as_ref()?
                .snapshot(self.snapshot_error(), client_id),
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

    fn publish(&mut self) -> bool {
        self.prune_selections();
        self.publish_context();
        self.recovery();
        let Some(model) = self.model.as_mut() else {
            return false;
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
            return false;
        }
        let Some(model) = &self.model else {
            return false;
        };
        // Snapshot construction includes all pane/project metadata. Every sidebar for a
        // client consumes identical state, so build and encode it once, not once per tab.
        let clients: BTreeSet<_> = model.frontend_clients.values().flatten().copied().collect();
        for client_id in clients {
            let state = self.state(client_id).expect("native model available");
            let mut payload = serde_json::to_string(&state).expect("serializing native state");
            let mut frontends = model
                .frontend_clients
                .iter()
                .filter(|(_, clients)| clients.contains(&client_id))
                .map(|(frontend, _)| *frontend)
                .peekable();
            while let Some(frontend) = frontends.next() {
                let payload = if frontends.peek().is_some() {
                    payload.clone()
                } else {
                    std::mem::take(&mut payload)
                };
                pipe_message_to_plugin(
                    MessageToPlugin::new(STATE)
                        .with_destination_plugin_id(frontend)
                        .with_payload(payload),
                );
            }
        }
        true
    }

    fn publish_context(&mut self) {
        if !self.granted || !self.context_ready || self.error.is_some() {
            return;
        }
        let Some(publication) = self
            .model
            .as_mut()
            .and_then(|model| model.reserve_context(self.server, self.id, self.context_reload))
        else {
            return;
        };
        self.context_reload = false;
        // Reserve in artifact-shared storage before creating an asynchronous native helper.
        // Reloads recover the reservation, so an older helper can never outrank its successor.
        self.save();
        if self.error.is_some() {
            return;
        }
        let json = serde_json::to_string(&publication).expect("serializing native context");
        run_command(
            &[
                &self.configuration["program"],
                "context-publish",
                "--json",
                &json,
            ],
            BTreeMap::from([(
                "native_context".to_owned(),
                publication.sequence.to_string(),
            )]),
        );
    }

    /// Offer the reconciled topology to the recovery gate and hand a due update to
    /// `bkpr session-record`. Only the native owner runs this; the shared model's
    /// `in_flight` sequence keeps per-client replicas to one helper at a time.
    /// Returns whether the shared model changed in a sidebar-visible way.
    fn recovery(&mut self) -> bool {
        if !self.granted || !self.context_ready || self.error.is_some() {
            return false;
        }
        let Some((tabs, manifest)) = &self.topology.reconciled else {
            return false;
        };
        let Some(model) = self.model.as_mut() else {
            return false;
        };
        let now = clock_millis();
        let mut error = None;
        if model.recovery.expire(now) {
            error = Some(Some(format!(
                "Session recovery helper did not report within {} s",
                RECOVERY_RESULT_MILLIS / 1_000
            )));
        }
        let mut command = None;
        let mut wait = None;
        if !model.recovery.disabled {
            let geometry = recovery_geometry(tabs, manifest);
            // Replicas must derive identical records: use the lowest client's focus.
            let focused = model.focus.values().next().copied();
            let view = model.recovery_workspaces(&geometry, tabs, focused);
            let session = (
                self.configuration["session"].as_str(),
                self.configuration["session_id"].as_str(),
            );
            match model.recovery.offer(view, now, session) {
                RecoveryDecision::Unchanged => {}
                RecoveryDecision::WaitUntil(deadline) => wait = Some(deadline),
                RecoveryDecision::Send(update) => match recovery_argument(&update) {
                    Ok(json) => {
                        // A lost result must still reach `expire` without another event.
                        wait = Some(now.saturating_add(RECOVERY_RESULT_MILLIS));
                        command = Some((update.sequence, json));
                    }
                    Err(message) => {
                        model.recovery.refuse(now);
                        wait = model.recovery.retry_at;
                        error = Some(Some(message));
                    }
                },
            }
        }
        let changed = error.is_some_and(|error| {
            let changed = model.recovery_error != error;
            model.recovery_error = error;
            changed
        });
        if changed {
            model.revision += 1;
        }
        if let Some(deadline) = wait {
            self.arm_recovery_timer(deadline, now);
        }
        if let Some((sequence, json)) = command {
            // Reserve the sequence in artifact-shared storage before spawning the helper.
            self.save();
            if self.error.is_none() {
                run_command(
                    &[
                        &self.configuration["program"],
                        "session-record",
                        "--json",
                        &json,
                    ],
                    BTreeMap::from([("native_recovery".to_owned(), sequence.to_string())]),
                );
            }
        }
        changed
    }

    fn arm_recovery_timer(&mut self, deadline: u64, now: u64) {
        if self
            .recovery_timer
            .is_none_or(|armed| deadline < armed || now >= armed)
        {
            self.recovery_timer = Some(deadline);
            // A small margin keeps the wake-up at or after the deadline.
            set_timeout((deadline.saturating_sub(now) + 5) as f64 / 1_000.0);
        }
    }

    fn recovery_result(&mut self, sequence: u64, code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        if !model
            .recovery
            .finish(sequence, code, stdout, clock_millis())
        {
            return;
        }
        let error = recovery_failure(code, stderr);
        if model.recovery_error != error {
            model.recovery_error = error;
            model.revision += 1;
        }
        // Persists the settlement and offers any change held behind the in-flight update.
        self.publish();
    }

    fn update_host_context(&mut self) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let Some(generation) = model.context_generation.checked_add(1) else {
            self.error = Some("Native context update sequence exhausted".to_owned());
            return;
        };
        model.context_generation = generation;
        let tabs: Vec<_> = model
            .tabs
            .iter()
            .filter_map(|(tab, workspace)| {
                model.workspaces.get(workspace).map(|name| HostContextTab {
                    tab_id: *tab,
                    workspace_id: workspace.clone(),
                    workspace_name: name.name.clone(),
                })
            })
            .collect();
        let payload = serde_json::json!({
            "session_id": self.configuration["session_id"],
            "generation": generation,
            "tabs": tabs,
        })
        .to_string();
        self.save();
        if self.error.is_none() {
            // Native host authenticates against this owner's private plugin configuration.
            post_message_to_plugin(PluginMessage {
                name: "bkpr_context_update".to_owned(),
                payload,
                ..PluginMessage::default()
            });
        }
    }

    fn schedule(&mut self) {
        if !self.reconcile_pending {
            self.reconcile_pending = true;
            set_timeout(0.03);
        }
    }

    fn adopt_context(&mut self, context: HostContext) -> Result<bool, &'static str> {
        let model = self.model.as_mut().ok_or("Native model is unavailable")?;
        let changed = model.adopt_context(context)?;
        self.context_ready = true;
        if changed {
            model.revision += 1;
        }
        // Consumed topology is replica-local and may predate another client's title.
        // Only an unconsumed PaneUpdate/TabUpdate pair can update terminal metadata.
        Ok(changed)
    }

    fn reconcile(&mut self) {
        if self.error.is_some() || !self.context_ready {
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
        let mut changed = model.reconcile_topology(&tabs, &manifest);
        for (id, name) in model.apply_restore_names() {
            rename_terminal_pane(id, &name);
            changed = true;
        }
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
            let initialized = cwd.is_ok()
                && command
                    .as_ref()
                    .is_ok_and(|command| !is_restore_helper(command));
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
        } else if self.recovery() {
            // Geometry-only changes (moves, tab renames) still update the recovery record.
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
                subscribe(&[
                    EventType::ModeUpdate,
                    EventType::InitialKeybinds,
                    EventType::CustomMessage,
                ]);
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
                self.error = Some("Native owner permissions denied".to_owned());
                // Queued CLI inputs are never replayed now; release their senders.
                for message in std::mem::take(&mut self.queued) {
                    if let PipeSource::Cli(source) = &message.source {
                        acknowledge_cli(source);
                    }
                }
            }
            Event::CustomMessage(name, payload)
                if self.granted && name == "bkpr_context" && payload.len() <= MAX_MESSAGE_BYTES =>
            {
                let Ok(context) = serde_json::from_str::<HostContext>(&payload) else {
                    return false;
                };
                let Ok(changed) = self.adopt_context(context) else {
                    return false;
                };
                self.reconcile();
                if changed {
                    self.request_agents();
                }
                self.publish();
            }
            Event::CustomMessage(name, payload)
                if self.granted && name == "mode_info" && payload.len() <= MAX_MESSAGE_BYTES =>
            {
                if let Ok(mode) = serde_json::from_str::<GuideMode>(&payload) {
                    self.guide_mode = Some(mode);
                }
            }
            Event::CustomMessage(name, payload)
                if self.granted
                    && name == "keybinds_info"
                    && payload.len() <= MAX_MESSAGE_BYTES =>
            {
                self.cache_keymap(payload);
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
            }
            Event::TabUpdate(tabs) if self.granted => {
                if self.topology.tabs(tabs) {
                    // PaneUpdate and TabUpdate already form a coherent screen event.
                    // Commit it now; a debounce delayed creation/focus and repeated
                    // synchronous focus reads before the same eventual reconciliation.
                    self.reconcile();
                } else if self.synchronize_focus(true) {
                    self.publish();
                }
                if self.topology.pending.is_none()
                    && (self.topology.ready.is_some() || self.topology.reconciled.is_some())
                {
                    for message in std::mem::take(&mut self.queued) {
                        self.pipe(message);
                    }
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
                if self
                    .recovery_timer
                    .is_some_and(|armed| clock_millis() >= armed)
                {
                    self.recovery_timer = None;
                }
                if self.recovery() {
                    self.publish();
                } else {
                    self.save();
                }
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
                if let Some(sequence) = context
                    .get("native_context")
                    .and_then(|sequence| sequence.parse::<u64>().ok())
                {
                    if let Some(model) = self.model.as_mut()
                        && model.context_sequence == sequence
                        && model.context_publisher == Some(self.id)
                    {
                        let error = (code != Some(0)).then(|| {
                            format!(
                                "Native context publication failed: {}",
                                crate::ui::clean(&String::from_utf8_lossy(
                                    &stderr[..stderr.len().min(4096)]
                                ))
                            )
                        });
                        if model.context_error != error {
                            model.context_error = error;
                            model.revision += 1;
                            self.publish();
                        }
                    }
                    return false;
                }
                if let Some(context) = context.get("native_agent") {
                    self.agent_response(code, &stdout, &stderr, context);
                    return false;
                }
                if let Some(sequence) = context
                    .get("native_recovery")
                    .and_then(|sequence| sequence.parse::<u64>().ok())
                {
                    self.recovery_result(sequence, code, &stdout, &stderr);
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

    fn action(
        &mut self,
        source: u32,
        action: Action,
        return_mode: Option<InputMode>,
        view: Option<&SidebarView>,
    ) -> Result<Option<u32>, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if !self.context_ready && !matches!(&action, Action::Snapshot | Action::RegisterFrontend) {
            return Err(
                "Native workspace context is unavailable until the host reports its bindings"
                    .to_owned(),
            );
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
                    self.create_tab(source, Some(workspace_id), None, None, return_mode, view)
                }
            }
            Action::CreateWorkspace { label, cwd } => {
                self.create_tab(source, None, label, cwd, return_mode, view)
            }
            Action::CreatePane { workspace_id, cwd } => {
                self.create_tab(source, Some(workspace_id), None, cwd, return_mode, view)
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
                self.update_host_context();
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
        }
    }

    fn create_tab(
        &mut self,
        source: u32,
        workspace: Option<String>,
        label: Option<String>,
        cwd: Option<String>,
        return_mode: Option<InputMode>,
        view: Option<&SidebarView>,
    ) -> Result<Option<u32>, String> {
        // This percentage bootstraps the layout only. The native shared-width group
        // restores remembered outer cells before publishing the new tab, including
        // when the requesting tab is temporarily clamped by a smaller terminal.
        let sidebar_percent = self.topology.sidebar_percent(source)?;
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
        let mut sidebar_configuration = tab_sidebar_configuration(&self.configuration);
        let mode = self.guide_mode.ok_or("Native input mode is unavailable")?;
        let keymap = self
            .keymap_cache
            .as_ref()
            .ok_or("Native keymap is unavailable")?;
        let default_view = SidebarView::default();
        let bootstrap = SidebarBootstrapRef {
            // Capture before native creation mutates topology or allocates a workspace.
            snapshot: model.snapshot(self.error.clone(), self.client_id),
            mode: return_mode.unwrap_or(mode.mode),
            base_mode: mode.base_mode,
            keymap,
            view: view.unwrap_or(&default_view),
        };
        sidebar_configuration.insert(
            "sidebar_bootstrap".to_owned(),
            serde_json::to_string(&bootstrap)
                .map_err(|error| format!("Cannot encode sidebar bootstrap: {error}"))?,
        );
        let created_workspace = workspace.is_none();
        let workspace = workspace.unwrap_or_else(|| model.workspace(name.clone()));
        // Preserve startup configuration/owner identity; geometry is inherited separately.
        let layout = bkpr::native_layout(
            &self.configuration["plugin_url"],
            &sidebar_configuration,
            &name,
            &cwd,
            sidebar_percent,
            Some((&workspace, &name)),
        )
        .map_err(|error| format!("Cannot create native terminal layout: {error}"))?;
        let tabs = new_tabs_with_layout(&layout);
        if tabs.is_empty() {
            if created_workspace {
                model.workspaces.remove(&workspace);
            }
            return Err("Zellij could not create the native terminal tab".to_owned());
        }
        for id in &tabs {
            model.tabs.insert(*id, workspace.clone());
            model.pending_tabs.insert(*id);
        }
        // Lifecycle events will populate and publish the actual native pane. Its paired
        // topology event reconciles immediately; there is no fabricated terminal or timer.
        Ok(None)
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == CONTROL {
            return self.control_pipe(&message);
        }
        if message.name == RESTORE_NAME {
            let PipeSource::Cli(source) = &message.source else {
                return false;
            };
            // Restored panes start before an authorized frontend grants the owner;
            // replaying the queue acknowledges the input then.
            if !self.granted {
                if self.queued.len() < 128 {
                    self.queued.push(message);
                } else {
                    acknowledge_cli(source);
                }
                return false;
            }
            acknowledge_cli(source);
            let Some(request) = message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= MAX_MESSAGE_BYTES)
                .and_then(|payload| serde_json::from_str::<RestoreNameRequest>(payload).ok())
            else {
                return false;
            };
            let Some(id) = terminal_id(&request.pane_id) else {
                return false;
            };
            self.recover();
            let Some(model) = self.model.as_mut() else {
                return false;
            };
            let named = model.restore_name(&request.key, id);
            let linked = match (request.harness, request.agent_id) {
                (Some(harness), Some(agent_id)) => {
                    model.restore_link(&request.key, id, &RestoreLink { harness, agent_id })
                }
                _ => false,
            };
            if !named && !linked {
                return false;
            }
            for (id, name) in model.apply_restore_names() {
                rename_terminal_pane(id, &name);
            }
            model.revision += 1;
            self.publish();
            return false;
        }
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
                        serde_json::to_string(&m.snapshot(self.snapshot_error(), self.client_id))
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
        // Resize publishes panes and tabs separately. Keep creation pending until its
        // source geometry is coherent rather than consuming the request with a transient error.
        if matches!(
            envelope.request.action,
            Action::CreateWorkspace { .. } | Action::CreatePane { .. }
        ) && (self.topology.pending.is_some()
            || (self.topology.ready.is_none() && self.topology.reconciled.is_none()))
        {
            if self.queued.len() < 128 {
                self.queued.push(message);
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
        let mut published = false;
        if receipt.is_some() && focus_changed {
            published = self.publish();
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
            let result = self.action(
                source,
                envelope.request.action,
                envelope.return_mode,
                envelope.view.as_ref(),
            );
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
            published = self.publish();
            receipt
        });
        if !published && let Some(state) = self.state(self.client_id) {
            let payload =
                serde_json::to_string(&state).expect("serializing native selection state");
            pipe_message_to_plugin(
                MessageToPlugin::new(STATE)
                    .with_destination_plugin_id(source)
                    .with_payload(payload),
            );
        }
        // Exactly one full STATE precedes this acknowledgement, even for registration
        // and duplicate requests whose canonical revision did not change.
        let response = NativeResponse {
            client: envelope.client,
            client_id: self.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: envelope.request.request_id,
                state: None,
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

/// The `bkpr restore-pane --key=…` helper a restore layout spawns for each leaf.
fn is_restore_helper(command: &[String]) -> bool {
    matches!(command, [_, helper, key, ..] if helper == "restore-pane" && key.starts_with("--key="))
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
    use bkpr::recovery_contract::SplitDirection;

    #[test]
    fn keymap_cache_compares_bytes_and_keeps_creation_refs_immutable() {
        std::fs::create_dir_all(KEYMAP_CACHE_DIRECTORY).unwrap();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let mut owner = Owner {
            model: Some(model()),
            selection_epoch: epoch.clone(),
            client_id: 7,
            ..Owner::default()
        };
        let keys: KeybindsVec = vec![(
            InputMode::Locked,
            vec![("Ctrl a".parse().unwrap(), Vec::new())],
        )];
        let first_payload = serde_json::to_string(&keys).unwrap();
        owner.cache_keymap(first_payload.clone());
        let first = owner.keymap_cache.clone().unwrap();
        let original_bytes = std::fs::read(first.path().unwrap()).unwrap();
        owner.cache_keymap(first_payload.clone());
        assert_eq!(owner.keymap_generation, 1);
        assert_eq!(owner.keymap_cache.as_ref(), Some(&first));
        assert_eq!(
            std::fs::read(first.path().unwrap()).unwrap(),
            original_bytes
        );
        assert_eq!(first.load("test-instance", 7).unwrap().0, keys);
        assert!(first.load("other-session", 7).is_none());
        assert!(first.load("test-instance", 8).is_none());

        let mut remapped = keys.clone();
        remapped[0].1[0].0 = "Ctrl b".parse().unwrap();
        let remapped_payload = serde_json::to_string(&remapped).unwrap();
        assert_eq!(first_payload.len(), remapped_payload.len());
        owner.cache_keymap(remapped_payload.clone());
        let second = owner.keymap_cache.clone().unwrap();
        assert_ne!(first, second);
        assert_eq!(second.load("test-instance", 7).unwrap().0, remapped);
        assert_eq!(first.load("test-instance", 7).unwrap().0, keys);
        owner.cache_keymap("{invalid".to_owned());
        assert_eq!(owner.keymap_cache.as_ref(), Some(&second));

        // Even a valid same-sized keymap cannot replace the referenced publication.
        let mut corrupt: KeymapCache = serde_json::from_slice(&original_bytes).unwrap();
        corrupt.payload = remapped_payload.clone();
        std::fs::write(first.path().unwrap(), serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert!(first.load("test-instance", 7).is_none());
        std::fs::write(
            first.path().unwrap(),
            vec![b' '; MAX_MESSAGE_BYTES * 2 + 4097],
        )
        .unwrap();
        assert!(first.load("test-instance", 7).is_none());

        let mut replacement = Owner {
            model: Some(model()),
            selection_epoch: format!("{epoch}:9"),
            client_id: 7,
            ..Owner::default()
        };
        replacement.cache_keymap(remapped_payload);
        let replaced = replacement.keymap_cache.unwrap();
        assert_ne!(replaced.source, second.source);
        assert_eq!(replaced.load("test-instance", 7).unwrap().0, remapped);
        // Matching payload/hash alone cannot authorize a different owner's publication.
        std::fs::write(
            replaced.path().unwrap(),
            std::fs::read(second.path().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(replaced.load("test-instance", 7).is_none());
        for reference in [first, second, replaced] {
            std::fs::remove_file(reference.path().unwrap()).unwrap();
        }
    }

    fn model() -> Model {
        Model::new(
            BTreeMap::from([
                ("session".to_owned(), "test".to_owned()),
                ("session_id".to_owned(), "test-session-id".to_owned()),
                ("context_owner".to_owned(), "private-test-owner".to_owned()),
            ]),
            "test-instance".to_owned(),
        )
    }

    impl Model {
        // Model-focused fixtures provide an explicit simulated host binding before topology.
        // Production never allocates stock workspace identities from PaneUpdate/TabUpdate.
        fn reconcile(&mut self, tabs: &[TabInfo], manifest: &PaneManifest) -> bool {
            let host_tabs: Vec<_> = tabs
                .iter()
                .map(|tab| {
                    let workspace_id = self
                        .tabs
                        .get(&tab.tab_id)
                        .cloned()
                        .or_else(|| {
                            manifest.panes.get(&tab.position).and_then(|panes| {
                                panes.iter().find_map(|pane| {
                                    self.panes.get(&pane.id).map(|pane| pane.workspace.clone())
                                })
                            })
                        })
                        .unwrap_or_else(|| format!("test-session-id:workspace:{}", tab.tab_id));
                    let workspace_name = self
                        .workspaces
                        .get(&workspace_id)
                        .map(|workspace| workspace.name.clone())
                        .unwrap_or_else(|| tab.name.clone());
                    HostContextTab {
                        tab_id: tab.tab_id,
                        workspace_id,
                        workspace_name,
                    }
                })
                .collect();
            let mut host_panes = Vec::new();
            for tab in &host_tabs {
                let position = tabs
                    .iter()
                    .find(|native| native.tab_id == tab.tab_id)
                    .unwrap()
                    .position;
                if let Some(panes) = manifest.panes.get(&position) {
                    for pane in panes.iter().filter(|pane| !pane.is_plugin) {
                        host_panes.push(HostContextPane {
                            pane_id: format!("terminal_{}", pane.id),
                            tab_id: tab.tab_id,
                            workspace_id: tab.workspace_id.clone(),
                            workspace_name: tab.workspace_name.clone(),
                            root_pid: None,
                        });
                    }
                } else if let Some(context) = &self.host_context {
                    host_panes.extend(
                        context
                            .panes
                            .iter()
                            .filter(|pane| pane.tab_id == tab.tab_id)
                            .cloned()
                            .map(|mut pane| {
                                pane.workspace_id = tab.workspace_id.clone();
                                pane.workspace_name = tab.workspace_name.clone();
                                pane
                            }),
                    );
                }
            }
            let changed = self
                .adopt_context(HostContext {
                    version: 1,
                    session_id: "test-session-id".to_owned(),
                    session_name: "test".to_owned(),
                    tabs: host_tabs,
                    panes: host_panes,
                })
                .unwrap();
            self.reconcile_topology(tabs, manifest) || changed
        }
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

    fn host_context(tabs: &[(usize, &str, &str)], panes: &[(u32, usize)]) -> HostContext {
        HostContext {
            version: 1,
            session_id: "test-session-id".to_owned(),
            session_name: "Native session".to_owned(),
            tabs: tabs
                .iter()
                .map(|(tab_id, id, name)| HostContextTab {
                    tab_id: *tab_id,
                    workspace_id: (*id).to_owned(),
                    workspace_name: (*name).to_owned(),
                })
                .collect(),
            panes: panes
                .iter()
                .map(|(id, tab)| {
                    let (_, workspace_id, workspace_name) =
                        tabs.iter().find(|(tab_id, _, _)| tab_id == tab).unwrap();
                    HostContextPane {
                        pane_id: format!("terminal_{id}"),
                        tab_id: *tab,
                        workspace_id: (*workspace_id).to_owned(),
                        workspace_name: (*workspace_name).to_owned(),
                        root_pid: Some(100 + id),
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn topology_waits_for_host_and_adopts_canonical_not_repository_identity() {
        let mut model = model();
        let tabs = [tab(10, 0), tab(20, 1)];
        let panes = manifest(&[
            (
                0,
                vec![
                    PaneInfo {
                        is_plugin: true,
                        ..pane(1)
                    },
                    pane(1),
                ],
            ),
            (1, vec![pane(2)]),
        ]);
        assert!(!model.reconcile_topology(&tabs, &panes));
        assert!(model.panes.is_empty());
        assert!(model.context_projection().is_none());
        let mut context = host_context(
            &[
                (10, "canonical-A", "First workspace"),
                (20, "canonical-B", "Second workspace"),
            ],
            &[(1, 10), (2, 20)],
        );
        context.panes.push(HostContextPane {
            pane_id: "plugin_1".to_owned(),
            tab_id: 10,
            workspace_id: "canonical-A".to_owned(),
            workspace_name: "First workspace".to_owned(),
            root_pid: None,
        });
        let mut foreign = context.clone();
        foreign.session_id = "foreign".to_owned();
        assert!(model.adopt_context(foreign).is_err());
        model.adopt_context(context).unwrap();
        assert!(model.reconcile_topology(&tabs, &panes));
        model.observe_cwd(1, "/same/repository".to_owned());
        model.observe_cwd(2, "/same/repository".to_owned());
        assert_eq!(model.panes[&1].workspace, "canonical-A");
        assert_eq!(model.panes[&2].workspace, "canonical-B");
        assert_eq!(model.workspaces["canonical-A"].name, "First workspace");
        assert_eq!(model.snapshot(None, 0).session, "Native session");
        assert_eq!(model.next_workspace, 0);
    }

    #[test]
    fn canonical_groups_follow_host_move_rename_and_close_not_stale_topology() {
        let mut model = model();
        let tabs = [tab(10, 0), tab(20, 1), tab(30, 2)];
        let old_panes = manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)]), (2, vec![pane(3)])]);
        model
            .adopt_context(host_context(
                &[
                    (10, "grouped", "Together"),
                    (20, "grouped", "Together"),
                    (30, "other", "Other"),
                ],
                &[(1, 10), (2, 20), (3, 30)],
            ))
            .unwrap();
        model.reconcile_topology(&tabs, &old_panes);
        assert_eq!(model.snapshot(None, 0).workspaces.len(), 2);
        model
            .adopt_context(host_context(
                &[
                    (10, "grouped", "Renamed"),
                    (20, "grouped", "Renamed"),
                    (30, "other", "Other"),
                ],
                &[(1, 10), (2, 30), (3, 30)],
            ))
            .unwrap();
        model.reconcile_topology(&tabs, &old_panes);
        assert_eq!(model.panes[&2].workspace, "other");
        assert_eq!(model.panes[&2].tab, 30);
        assert_eq!(model.workspaces["grouped"].name, "Renamed");
        model
            .adopt_context(host_context(&[(30, "other", "Other")], &[(2, 30), (3, 30)]))
            .unwrap();
        model.reconcile_topology(&tabs, &old_panes);
        assert!(!model.panes.contains_key(&1));
        assert!(!model.workspaces.contains_key("grouped"));
        assert_eq!(model.panes[&2].workspace, "other");
    }

    #[test]
    fn a_closed_native_tab_id_can_host_a_new_workspace() {
        let mut model = model();
        let first = host_context(&[(10, "first", "First")], &[(1, 10)]);
        model.adopt_context(first.clone()).unwrap();
        model.reconcile_topology(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.closed_tabs.insert(10);
        model.close_pane(1);
        model.tabs.remove(&10);
        model.adopt_context(first).unwrap();
        assert!(!model.tabs.contains_key(&10));
        model.adopt_context(host_context(&[], &[])).unwrap();
        model
            .adopt_context(host_context(&[(10, "second", "Second")], &[(2, 10)]))
            .unwrap();
        model.reconcile_topology(&[tab(10, 0)], &manifest(&[(0, vec![pane(2)])]));
        let projection = model.context_projection().unwrap();
        assert_eq!(projection.panes[0].pane_id, "terminal_2");
        assert_eq!(projection.panes[0].workspace_id, "second");
        assert_eq!(projection.workspaces[0].name, "Second");
        assert!(!model.panes.contains_key(&1));
    }

    #[test]
    fn publication_tracks_foreground_ids_names_without_frontends_or_focus() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        let mut agent = execution(1);
        agent.agent_id = Some("registered-agent".to_owned());
        model.agents.insert(1, agent);
        let ordinary = model.panes.get_mut(&2).unwrap();
        ordinary.name = "Build shell".to_owned();
        ordinary.renamed = true;
        let first = model.reserve_context(99, 5, false).unwrap();
        assert!(model.frontends.is_empty());
        assert!(model.focus.is_empty());
        assert_eq!(first.panes[0].agent_id.as_deref(), Some("registered-agent"));
        assert_eq!(first.panes[1].agent_id, None);
        assert_eq!(first.panes[1].pane_name, "Build shell");
        let snapshot = model.snapshot(None, 0);
        for (published, pane) in first.panes.iter().zip(&snapshot.panes) {
            assert_eq!(published.agent_id, pane.agent_id);
            assert_eq!(published.pane_name, pane.name);
        }
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("private-test-owner")
        );
        assert!(model.reserve_context(99, 5, false).is_none());
        model.record_focus(7, 2);
        assert!(model.reserve_context(99, 5, false).is_none());
        model.invalidate_agent(1);
        let next = model.reserve_context(99, 5, false).unwrap();
        assert!(next.sequence > first.sequence);
        assert_eq!(next.panes[0].agent_id, None);
        model.close_pane(1);
        let closed = model.reserve_context(99, 5, false).unwrap();
        assert_eq!(closed.panes.len(), 1);
    }

    #[test]
    fn publication_reservations_survive_reload_and_rotate_owner_before_dispatch() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let reserved = model.reserve_context(99, 5, false).unwrap();
        let bytes = serde_json::to_vec(&model).unwrap();
        let mut reloaded = Model::decode(&bytes).unwrap();
        assert_eq!(reloaded.context_sequence, reserved.sequence);
        assert!(reloaded.reserve_context(99, 5, false).is_none());
        let same_id_reload = reloaded.reserve_context(99, 5, true).unwrap();
        assert!(same_id_reload.sequence > reserved.sequence);
        let replacement = reloaded.reserve_context(99, 6, false).unwrap();
        assert!(replacement.sequence > same_id_reload.sequence);
        assert_eq!(replacement.owner, 6);
        assert_eq!(replacement.panes, reserved.panes);
        let persisted = Model::decode(&serde_json::to_vec(&reloaded).unwrap()).unwrap();
        assert_eq!(persisted.context_sequence, replacement.sequence);
        assert_eq!(persisted.context_publisher, Some(6));
    }

    #[test]
    fn strict_role_configuration_keeps_context_authority_owner_only() {
        let mut configuration = BTreeMap::from([
            ("role".to_owned(), "native-owner".to_owned()),
            ("session".to_owned(), "test".to_owned()),
            ("session_id".to_owned(), "test-session-id".to_owned()),
            ("context_owner".to_owned(), "private-test-owner".to_owned()),
            ("program".to_owned(), "/bin/bkpr".to_owned()),
            ("initial_cwd".to_owned(), "/work".to_owned()),
            ("plugin_url".to_owned(), "file:/bkpr.wasm".to_owned()),
            ("sidebar_width".to_owned(), "28".to_owned()),
        ]);
        assert!(configuration_valid(&configuration, "native-owner"));
        configuration.insert("role".to_owned(), "sidebar".to_owned());
        assert!(!configuration_valid(&configuration, "sidebar"));
        configuration.remove("context_owner");
        assert!(configuration_valid(&configuration, "sidebar"));
        configuration.insert("unexpected".to_owned(), "value".to_owned());
        assert!(!configuration_valid(&configuration, "sidebar"));
    }

    #[test]
    fn grouped_tab_layout_prebinds_workspace_before_terminal_creation() {
        let configuration = BTreeMap::from([
            ("role".to_owned(), "sidebar".to_owned()),
            ("session_id".to_owned(), "test-session-id".to_owned()),
        ]);
        let layout = bkpr::native_layout(
            "file:/bkpr.wasm",
            &configuration,
            "Together",
            "/same/repository",
            20,
            Some(("canonical-shared-workspace", "Together")),
        )
        .unwrap();
        assert!(layout.contains("bkpr_workspace_id=\"canonical-shared-workspace\""));
        assert!(layout.contains("bkpr_workspace_name=\"Together\""));
        assert!(!layout.contains("context_owner"));
    }

    #[test]
    fn new_tab_sizing_uses_requesting_sidebar_outer_width_and_latest_geometry() {
        let mut source_tab = tab(10, 0);
        source_tab.display_area_columns = 200;
        source_tab.viewport_columns = 162;
        let mut other_tab = tab(20, 1);
        other_tab.display_area_columns = 300;
        let mut sidebar = PaneInfo {
            id: 7,
            is_plugin: true,
            pane_columns: 38,
            pane_content_columns: 37,
            ..PaneInfo::default()
        };
        let other_sidebar = PaneInfo {
            id: 8,
            is_plugin: true,
            pane_columns: 51,
            pane_content_columns: 50,
            ..PaneInfo::default()
        };
        let mut topology = Topology::default();
        topology.panes(manifest(&[
            (0, vec![sidebar.clone()]),
            (1, vec![other_sidebar.clone()]),
        ]));
        assert!(topology.tabs(vec![source_tab.clone(), other_tab.clone()]));
        assert_eq!(topology.sidebar_percent(7), Ok(19));
        assert_eq!(topology.sidebar_percent(8), Ok(17));
        topology.reconciled = topology.ready.take();

        sidebar.pane_columns = 47;
        sidebar.pane_content_columns = 46;
        topology.panes(manifest(&[(0, vec![sidebar]), (1, vec![other_sidebar])]));
        assert!(topology.sidebar_percent(7).is_err());
        assert!(topology.tabs(vec![source_tab, other_tab]));
        // 46 content columns would round to 23%, rather than the outer width's 24%.
        assert_eq!(topology.sidebar_percent(7), Ok(24));
        assert!(topology.sidebar_percent(99).is_err());
    }

    #[test]
    fn new_tab_sizing_rejects_missing_or_incoherent_source_geometry() {
        let mut source_tab = tab(10, 0);
        source_tab.display_area_columns = 200;
        let mut sidebar = PaneInfo {
            id: 7,
            is_plugin: true,
            pane_columns: 38,
            ..PaneInfo::default()
        };
        let mut topology = Topology::default();
        for invalid in 0..6 {
            sidebar.is_plugin = invalid != 0;
            sidebar.is_floating = invalid == 1;
            sidebar.is_suppressed = invalid == 2;
            sidebar.exited = invalid == 3;
            sidebar.pane_columns = if invalid == 4 { 0 } else { 38 };
            source_tab.display_area_columns = if invalid == 5 { 20 } else { 200 };
            topology.panes(manifest(&[(0, vec![sidebar.clone()])]));
            topology.tabs(vec![source_tab.clone()]);
            assert!(topology.sidebar_percent(7).is_err());
        }
        sidebar.pane_columns = 38;
        source_tab.display_area_columns = 200;
        topology.panes(manifest(&[(0, vec![sidebar])]));
        topology.tabs(vec![source_tab.clone()]);
        assert_eq!(topology.sidebar_percent(7), Ok(19));
        topology.reconciled = topology.ready.take();

        topology.panes(manifest(&[(1, vec![pane(1)])]));
        assert!(!topology.tabs(vec![source_tab.clone()]));
        assert!(topology.sidebar_percent(7).is_err());
        topology.panes(manifest(&[(0, vec![pane(7)])]));
        topology.tabs(vec![source_tab.clone()]);
        assert!(topology.sidebar_percent(7).is_err());
        assert!(!topology.tabs(vec![source_tab]));
        assert!(topology.sidebar_percent(7).is_err());
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
            "head_sha": "local-current",
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
                "branch": "main", "local_head_sha": "local-current",
                "checked_ref": "origin/main", "truncated": false
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
        model.sync_project_colors();
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

    fn assign_repository(model: &mut Model, pane: u32, cwd: &str, repository: &str) {
        model.observe_cwd(pane, cwd.to_owned());
        model.sync_project_watches(5);
        let project = serde_json::from_value(serde_json::json!({
            "name": "same-name", "repository": true, "repository_id": repository,
            "branch": "main"
        }))
        .unwrap();
        assert!(model.apply_project(project_update(model, cwd, project)));
    }

    #[test]
    fn project_slots_are_session_wide_and_preserve_formerly_colliding_repositories() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[
                (0, [1, 2, 3, 5, 6, 7].into_iter().map(pane).collect()),
                (1, std::iter::once(4).chain(8..=14).map(pane).collect()),
            ]),
        );
        for id in 1..=13 {
            assign_repository(
                &mut model,
                id,
                &format!("/worktree-{id}"),
                &format!("/repo-{}/.git", id - 1),
            );
        }
        assign_repository(&mut model, 14, "/linked-worktree", "/repo-0/.git");
        let before = model.project_colors.clone();
        assert_eq!(before.len(), 13);
        assert_eq!(before.values().copied().collect::<BTreeSet<_>>().len(), 13);
        // These actual canonical IDs both mapped to old FNV slot1 (ANSI5).
        let old_slot = |id: &str| {
            id.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            }) % 6
        };
        assert_eq!(old_slot("/repo-1/.git"), old_slot("/repo-3/.git"));
        assert_ne!(before["/repo-1/.git"], before["/repo-3/.git"]);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.workspaces.len(), 2);
        assert_ne!(
            snapshot.panes[1].workspace_id,
            snapshot.panes[3].workspace_id
        );
        assert_eq!(snapshot.panes[0].project, snapshot.panes[13].project);
        let mut theme = crate::palette::Theme::default();
        theme.prepare_snapshot(&snapshot);
        let styles: BTreeSet<_> = snapshot.panes[..12]
            .iter()
            .map(|pane| crate::ui::project_color(pane.project.as_ref(), &theme))
            .collect();
        assert_eq!(styles.len(), 12);
        assert_eq!(
            crate::ui::project_color(snapshot.panes[12].project.as_ref(), &theme),
            theme.muted
        );
        for workspace in &snapshot.workspaces {
            for project in &workspace.projects {
                assert_eq!(project.color_slot, before.get(&project.id).copied());
            }
        }

        // Moving a pane across workspaces, tab reordering and labels do not reassign.
        model.reconcile(
            &[tab(20, 0), tab(10, 1)],
            &manifest(&[
                (0, (8..=14).chain(std::iter::once(1)).map(pane).collect()),
                (1, (2..=7).map(pane).collect()),
            ]),
        );
        model
            .workspaces
            .values_mut()
            .for_each(|workspace| workspace.name = "renamed".into());
        model.panes.get_mut(&1).unwrap().name = "renamed pane".into();
        model.sync_project_watches(5);
        assert_eq!(model.project_colors, before);

        // The last-pane reservation survives both branch invalidation and an
        // unresolved worktree CWD change, including owner recovery mid-discovery.
        model.invalidate_project("/worktree-2");
        model.observe_cwd(2, "/another-linked-worktree".into());
        model.sync_project_watches(5);
        model = Model::decode(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(model.project_colors, before);
        assign_repository(&mut model, 2, "/another-linked-worktree", "/repo-1/.git");
        let metadata = model.projects.get_mut("/another-linked-worktree").unwrap();
        metadata.name = "repository renamed".into();
        metadata.branch = Some("feature/other".into());
        model.sync_project_colors();
        assert_eq!(model.project_colors, before);
        let recovered = Model::decode(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(recovered.snapshot(None, 1), model.snapshot(None, 1));

        // Close both copies of repo0 and repo2. Their colors become available,
        // but all survivors (including an unrepresentable native slot) stay put.
        model.reconcile(
            &[tab(20, 0), tab(10, 1)],
            &manifest(&[
                (0, (8..=13).map(pane).collect()),
                (1, [2, 4, 5, 6, 7, 15, 16].into_iter().map(pane).collect()),
            ]),
        );
        model.sync_project_watches(5);
        assert!(!model.project_colors.contains_key("/repo-0/.git"));
        assert!(!model.project_colors.contains_key("/repo-2/.git"));
        for (id, slot) in &model.project_colors {
            assert_eq!(Some(slot), before.get(id));
        }
        assign_repository(&mut model, 15, "/new-project", "/new-project/.git");
        assign_repository(&mut model, 16, "/another-project", "/another-project/.git");
        assert_eq!(
            model.project_colors["/new-project/.git"],
            before["/repo-0/.git"]
        );
        assert_eq!(
            model.project_colors["/another-project/.git"],
            before["/repo-2/.git"]
        );
        assert_eq!(
            model
                .project_colors
                .values()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            13
        );
    }

    // LEGACY_COMPAT(next-major): recover old persisted models without color fields.
    #[test]
    fn legacy_owner_recovery_seeds_shared_repository_assignments() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0), tab(20, 1)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2), pane(3)])]),
        );
        assign_repository(&mut model, 1, "/repo", "/repo/.git");
        assign_repository(&mut model, 2, "/linked", "/repo/.git");
        assign_repository(&mut model, 3, "/other", "/other/.git");
        let mut wire = serde_json::to_value(&model).unwrap();
        wire.as_object_mut().unwrap().remove("project_colors");
        wire.as_object_mut()
            .unwrap()
            .remove("project_color_repositories");
        let recovered = Model::decode(&serde_json::to_vec(&wire).unwrap()).unwrap();
        assert_eq!(recovered.revision, model.revision + 1);
        let snapshot = recovered.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].project, snapshot.panes[1].project);
        assert_ne!(
            snapshot.panes[0].project.as_ref().unwrap().color_slot,
            snapshot.panes[2].project.as_ref().unwrap().color_slot
        );
        assert!(
            snapshot
                .panes
                .iter()
                .all(|pane| pane.project.as_ref().unwrap().color_slot.is_some())
        );
    }

    #[test]
    fn project_reservations_release_after_known_repository_or_directory_changes() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3)])]),
        );
        assign_repository(&mut model, 1, "/repo", "/repo/.git");
        assign_repository(&mut model, 2, "/linked", "/repo/.git");
        let original = model.project_colors["/repo/.git"];
        assign_repository(&mut model, 1, "/other", "/other/.git");
        let other = model.project_colors["/other/.git"];
        assert_ne!(original, other);
        assert_eq!(model.project_colors["/repo/.git"], original);
        model.observe_cwd(2, "/plain-directory".into());
        model.sync_project_watches(5);
        assert_eq!(model.project_colors["/repo/.git"], original);
        let directory = serde_json::from_value(serde_json::json!({
            "name": "directory", "repository": false
        }))
        .unwrap();
        assert!(model.apply_project(project_update(&model, "/plain-directory", directory)));
        assert!(!model.project_colors.contains_key("/repo/.git"));
        assert_eq!(model.project_colors["/other/.git"], other);
        assert!(model.snapshot(None, 1).panes[1].project.is_none());
        assign_repository(&mut model, 3, "/new", "/new/.git");
        assert_eq!(model.project_colors["/new/.git"], original);
        assert_eq!(model.project_colors["/other/.git"], other);
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
    fn context_updates_do_not_replay_a_replicas_consumed_titles() {
        for context_changed in [false, true] {
            let tabs = vec![tab(10, 0)];
            let mut terminal = pane(1);
            terminal.title = "Previous task".to_owned();
            let old_manifest = manifest(&[(0, vec![terminal.clone()])]);
            let mut model = model();
            model.reconcile(&tabs, &old_manifest);
            let mut context = model.host_context.clone().unwrap();
            terminal.title = "Current task".to_owned();
            model.reconcile(&tabs, &manifest(&[(0, vec![terminal])]));
            if context_changed {
                context.session_name = "Renamed session".to_owned();
            }
            let mut owner = Owner {
                model: Some(model),
                context_ready: true,
                topology: Topology {
                    reconciled: Some((tabs, old_manifest)),
                    ..Topology::default()
                },
                ..Owner::default()
            };
            assert_eq!(owner.adopt_context(context), Ok(context_changed));
            assert_eq!(
                owner.model.as_ref().unwrap().snapshot(None, 0).panes[0].name,
                "Current task"
            );
            assert!(owner.topology.ready.is_none());
            assert!(owner.topology.reconciled.is_some());
        }
    }

    #[test]
    fn initial_context_preserves_waiting_fresh_topology() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let context = model.host_context.take().unwrap();
        let mut terminal = pane(1);
        terminal.title = "Fresh task".to_owned();
        let mut owner = Owner {
            model: Some(model),
            topology: Topology {
                ready: Some((vec![tab(10, 0)], manifest(&[(0, vec![terminal])]))),
                ..Topology::default()
            },
            ..Owner::default()
        };
        owner.reconcile();
        assert!(owner.topology.ready.is_some());
        assert!(owner.adopt_context(context).unwrap());
        assert!(owner.context_ready);
        let (tabs, manifest) = owner.topology.ready.take().unwrap();
        let model = owner.model.as_mut().unwrap();
        assert!(model.reconcile_topology(&tabs, &manifest));
        assert_eq!(model.snapshot(None, 0).panes[0].name, "Fresh task");
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
        model.reconcile(&[tab(10, 0)], &PaneManifest::default());
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
    fn pending_creation_replays_without_changing_the_existing_sidebar_snapshot() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.record_focus(1, 1);
        model.panes.get_mut(&1).unwrap().name = "Custom terminal".to_owned();
        model.panes.get_mut(&1).unwrap().renamed = true;
        model.observe_cwd(1, "/repo".into());
        assert!(model.sync_project_watches(5));
        assert!(model.apply_project(project_update(
            &model,
            "/repo",
            passing_project("current-sha"),
        )));
        let watches = model.project_watches.clone();
        let projects = model.projects.clone();
        let original = model.snapshot(None, 1);
        let existing = model.panes[&1].workspace.clone();
        let created = model.workspace("Created workspace".to_owned());
        model.tabs.insert(20, created.clone());
        model.pending_tabs.insert(20);
        model.tabs.insert(30, existing.clone());
        model.pending_tabs.insert(30);
        assert_eq!(model.snapshot(None, 1), original);

        // Another owner replica or a new sidebar can recover while creation is in flight.
        model = Model::decode(&serde_json::to_vec(&model).unwrap()).unwrap();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        assert_eq!(model.workspaces[&created].name, "Created workspace");
        assert_eq!(model.snapshot(None, 1), original);
        model.reconcile(
            &[tab(10, 0), tab(20, 1), tab(30, 2)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![]), (2, vec![])]),
        );
        assert_eq!(model.pending_tabs, BTreeSet::from([20, 30]));
        assert_eq!(model.snapshot(None, 1), original);

        model.reconcile(
            &[tab(10, 0), tab(20, 1), tab(30, 2)],
            &manifest(&[(0, vec![pane(1)]), (1, vec![pane(2)]), (2, vec![pane(3)])]),
        );
        assert!(model.pending_tabs.is_empty());
        assert_eq!(model.panes[&2].workspace, created);
        assert_eq!(model.panes[&3].workspace, existing);
        assert!(model.project_watches == watches);
        assert!(model.projects == projects);
        assert!(!model.sync_project_watches(5));
        let complete = model.snapshot(None, 1);
        assert_eq!(complete.workspaces.len(), 2);
        assert_eq!(complete.panes.len(), 3);
        assert_eq!(complete.panes[0], original.panes[0]);
        assert_eq!(complete.focused_pane_id, original.focused_pane_id);
        let replayed: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&complete).unwrap()).unwrap();
        assert_eq!(replayed, complete);
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
            agent_id: None,
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
            "head_sha": sha,
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
                "branch": "feature", "local_head_sha": sha,
                "checked_ref": "origin/feature", "truncated": false
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
    fn remote_ci_validates_local_identity_in_results_and_recovered_cache() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_cwd(1, "/repo".into());
        model.sync_project_watches(5);
        let mut project = passing_project("local-head");
        project.ci.as_mut().unwrap().head_sha = "remote-head".into();
        assert!(model.apply_project(project_update(&model, "/repo", project.clone())));
        let recovered = Model::decode(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(recovered.snapshot(None, 1), model.snapshot(None, 1));
        let ci = recovered.projects["/repo"].ci.as_ref().unwrap();
        assert_eq!(ci.head_sha, "remote-head");
        assert_eq!(ci.local_head_sha, "local-head");
        assert_eq!(
            recovered.projects["/repo"].head_sha.as_deref(),
            Some("local-head")
        );

        for case in 0..3 {
            let mut value = serde_json::to_value(&model).unwrap();
            let metadata = &mut value["projects"]["/repo"];
            match case {
                0 => {
                    metadata["ci"]
                        .as_object_mut()
                        .unwrap()
                        .remove("local_head_sha");
                }
                1 => metadata["ci"]["local_head_sha"] = serde_json::json!("wrong-local-head"),
                2 => {
                    metadata.as_object_mut().unwrap().remove("head_sha");
                }
                _ => unreachable!(),
            }
            let invalid_project: Project = serde_json::from_value(metadata.clone()).unwrap();
            let recovered = Model::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(recovered.projects["/repo"].ci.is_none(), "case {case}");
            assert_eq!(recovered.projects["/repo"].branch, project.branch);
            assert_eq!(
                recovered.projects["/repo"].branch_changes,
                project.branch_changes
            );
            assert!(!recovered.project_result_current("/repo", &model.project_watches["/repo"]));
            model.apply_project(project_update(&model, "/repo", invalid_project));
            assert!(model.projects["/repo"].ci.is_none(), "case {case}");
            model.apply_project(project_update(&model, "/repo", project.clone()));
        }
    }

    #[test]
    fn ci_refresh_requires_registered_client_and_current_watched_commit_without_reticketing() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_cwd(1, "/repo".into());
        model.sync_project_watches(5);
        let mut project = passing_project("local-head");
        project.ci.as_mut().unwrap().head_sha = "old".into();
        model.apply_project(project_update(&model, "/repo", project));
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
        let mut local_target = target.clone();
        local_target.head_sha = "local-head".into();
        assert!(!model.ci_refresh_current(7, 1, &local_target));
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
    fn new_launch_identity_refreshes_the_same_harness_popup() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let mut agent = execution(1);
        agent.agent_id = Some("0123456789abcdef0123456789abcdef".to_owned());
        let ticket = model.agent_ticket(5, 1, 1_000).unwrap();
        assert!(
            model
                .apply_agents(
                    &ticket,
                    InspectResponse {
                        executions: vec![agent.clone()]
                    },
                    1_001
                )
                .unwrap()
        );
        assert_eq!(model.snapshot(None, 1).panes[0].agent_id, agent.agent_id);

        agent.agent_id = Some("fedcba9876543210fedcba9876543210".to_owned());
        let ticket = model.agent_ticket(5, 1, 2_000).unwrap();
        assert!(
            model
                .apply_agents(
                    &ticket,
                    InspectResponse {
                        executions: vec![agent.clone()]
                    },
                    2_001
                )
                .unwrap()
        );
        assert_eq!(model.snapshot(None, 1).panes[0].agent_id, agent.agent_id);
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
                bkpr::ci_contract::CiStatus::Passed
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
                bkpr::ci_contract::CiStatus::Passed
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
    fn restore_helper_command_stays_unknown_until_requeried() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.panes.get_mut(&1).unwrap().metadata_initialized = true;
        // Zellij reports a restored pane's spawn command even after its exec into the shell.
        let helper = [
            "/nix/store/x-bkpr/bin/bkpr",
            "restore-pane",
            "--key=w0t0p0",
            "--name=a",
        ];
        model.observe_command(1, helper.map(str::to_owned).to_vec());
        let pane = &model.panes[&1];
        assert!(pane.command.is_empty() && !pane.shell);
        assert!(
            !pane.metadata_initialized,
            "re-queried on the next reconcile"
        );
        assert_eq!(
            model.snapshot(None, 1).panes[0].description,
            "Native Zellij terminal · execution unknown"
        );
        assert!(model.observe_command(1, vec!["fish".to_owned()]));
        assert!(model.panes[&1].shell);
        // Only the helper's own argv shape is unknown; other `restore-pane` words are kept.
        assert!(model.observe_command(1, vec!["grep".to_owned(), "restore-pane".to_owned()]));
        assert_eq!(model.panes[&1].command, ["grep", "restore-pane"]);
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
    fn publication_carries_harness_status_but_not_generic_status_or_progress() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        let mut working = program("working", "omp");
        working.msg = Some("Running tests".to_owned());
        working.progress = Some(10);
        inspect_programs(&mut model, vec![execution(1)], vec![working.clone()], 1_000);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![program("working", "cargo")],
            })
            .unwrap();
        let first = model.reserve_context(99, 5, false).unwrap();
        assert_eq!(
            first.panes[0].status,
            Some(PaneStatus {
                state: Status::Working,
                attention: None,
                message: Some("Running tests".to_owned()),
            })
        );
        assert_eq!(first.panes[1].status, None);

        working.progress = Some(90);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![working.clone()],
            })
            .unwrap();
        assert!(model.reserve_context(99, 5, false).is_none());

        let mut blocked = program("blocked", "omp");
        blocked.kind = Some("permission".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![blocked],
            })
            .unwrap();
        let next = model.reserve_context(99, 5, false).unwrap();
        assert_eq!(
            next.panes[0].status,
            Some(PaneStatus {
                state: Status::Blocked,
                attention: Some(Attention::Permission),
                message: None,
            })
        );
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
    fn generic_pushed_status_is_not_borrowed_by_late_process_identification() {
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
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("claude"));
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].attention, None);
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("done", "cargo")],
            })
            .unwrap();
        model.observe_roots(BTreeMap::new());
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Done);
        assert_eq!(snapshot.panes[0].harness, None);
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
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
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("claude"));
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].description, "Compiled successfully");
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
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

    #[test]
    fn arbitrary_reporters_keep_metadata_but_never_enter_harness_summaries() {
        let mut model = model();
        let mut terminal = pane(1);
        terminal.title.clear();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![terminal])]));
        model.observe_cwd(1, "/repo".to_owned());
        model
            .projects
            .insert("/repo".to_owned(), passing_project("head"));
        let metadata = model.snapshot(None, 1).panes.remove(0);
        for app in ["cargo", "git", "shell", "unknown-app"] {
            for state in ["working", "blocked", "error", "done"] {
                let mut record = program(state, app);
                record.kind = (state == "blocked").then(|| "permission".to_owned());
                record.msg = Some("Useful program message".to_owned());
                model
                    .apply_program_status(ProgramStatusUpdate {
                        pane_id: 1,
                        records: vec![record.clone()],
                    })
                    .unwrap();
                let snapshot = model.snapshot(None, 1);
                let pane = &snapshot.panes[0];
                assert_eq!(pane.harness, None, "{app} {state}");
                assert_eq!(pane.status, record.status());
                assert_eq!(pane.attention, None);
                assert_eq!(pane.name, app);
                assert_eq!(pane.description, "Useful program message");
                assert_eq!(pane.branch, metadata.branch);
                assert_eq!(pane.branch_changes, metadata.branch_changes);
                assert_eq!(pane.project, metadata.project);
                assert_eq!(pane.ci, metadata.ci);
                assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
                assert_eq!(snapshot.workspaces[0].attention, None);
            }
        }
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![program("working", "claude-code")],
            })
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        let pane = &snapshot.panes[0];
        assert_eq!(pane.harness.as_deref(), Some("claude-code"));
        assert_eq!(pane.status, Status::Working);
        assert_eq!(pane.branch, metadata.branch);
        assert_eq!(pane.branch_changes, metadata.branch_changes);
        assert_eq!(pane.project, metadata.project);
        assert_eq!(pane.ci, metadata.ci);
    }

    #[test]
    fn mixed_reports_project_harness_status_independently_of_generic_dominance() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        let mut generic = program("blocked", "cargo");
        generic.kind = Some("question".to_owned());
        generic.title = Some("Compile project".to_owned());
        generic.msg = Some("Compiler prompt".to_owned());
        generic.progress = Some(75);
        let mut harness = program("working", "claude-code");
        harness.id = "agent".to_owned();
        for (state, kind, status, attention) in [
            ("working", None, Status::Working, None),
            ("idle", None, Status::Idle, None),
            (
                "blocked",
                Some("permission"),
                Status::Blocked,
                Some(Attention::Permission),
            ),
            ("error", None, Status::Blocked, Some(Attention::Error)),
        ] {
            harness.state = state.to_owned();
            harness.kind = kind.map(str::to_owned);
            // Keep the generic record last so it remains the raw dominant record
            // even when both records are blocked.
            assert!(
                model
                    .apply_program_status(ProgramStatusUpdate {
                        pane_id: 1,
                        records: vec![harness.clone(), generic.clone()],
                    })
                    .unwrap()
            );
            let snapshot = model.snapshot(None, 1);
            assert_eq!(snapshot.panes[0].harness.as_deref(), Some("claude-code"));
            assert_eq!(snapshot.panes[0].status, status);
            assert_eq!(snapshot.panes[0].attention, attention);
            assert_eq!(snapshot.panes[0].name, "Compile project");
            assert_eq!(snapshot.panes[0].description, "Compiler prompt · 75%");
            assert_eq!(snapshot.workspaces[0].status, status);
            assert_eq!(snapshot.workspaces[0].attention, attention);
        }
        model.observe_roots(BTreeMap::from([(1, 101)]));
        inspect(&mut model, vec![execution(1)], 1_000);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![generic],
            })
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("claude"));
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].attention, None);
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
        assert_eq!(snapshot.workspaces[0].attention, None);
    }

    #[test]
    fn app_less_reports_require_live_recognized_process_identity() {
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        model.observe_roots(BTreeMap::from([(1, 101)]));
        model.observe_command(1, vec!["bash".to_owned()]);
        let mut anonymous = program("blocked", "claude");
        anonymous.app = None;
        anonymous.kind = Some("permission".to_owned());
        let mut generic = program("error", "cargo");
        generic.id = "compiler".to_owned();
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![anonymous.clone(), generic.clone()],
            })
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness, None);
        assert_eq!(snapshot.panes[0].attention, None);
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);

        let ticket = model.agent_ticket(5, 1, 1_000).unwrap();
        assert!(
            model
                .apply_agents(
                    &ticket,
                    InspectResponse {
                        executions: vec![execution(1)]
                    },
                    1_001,
                )
                .unwrap()
        );
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("claude"));
        assert_eq!(snapshot.panes[0].status, Status::Blocked);
        assert_eq!(snapshot.panes[0].attention, Some(Attention::Permission));
        assert_eq!(
            snapshot.workspaces[0].attention,
            Some(Attention::Permission)
        );

        // A new explicit unrelated app must not retain the anonymous harness status.
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![generic],
            })
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].attention, None);
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![],
            })
            .unwrap();
        assert_eq!(model.snapshot(None, 1).panes[0].status, Status::Unknown);
        inspect(&mut model, Vec::new(), 1_500);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness, None);
        assert_eq!(snapshot.panes[0].status, Status::Idle);

        let mut next = execution(1);
        next.harness = "goose".to_owned();
        next.identity = "next-birth".to_owned();
        inspect(&mut model, vec![next], 2_000);
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].harness.as_deref(), Some("goose"));
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[0].attention, None);
    }

    #[test]
    fn mixed_workspace_ignores_generic_attention_and_shell_idle() {
        let mut model = model();
        model.reconcile(
            &[tab(10, 0)],
            &manifest(&[(0, vec![pane(1), pane(2), pane(3)])]),
        );
        model.observe_roots(BTreeMap::from([(1, 101)]));
        model.observe_command(3, vec!["bash".to_owned()]);
        inspect(&mut model, vec![execution(1)], 1_000);
        let mut generic = program("blocked", "cargo");
        generic.kind = Some("permission".to_owned());
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 2,
                records: vec![generic],
            })
            .unwrap();
        let snapshot = model.snapshot(None, 1);
        assert_eq!(snapshot.panes[0].status, Status::Unknown);
        assert_eq!(snapshot.panes[1].harness, None);
        assert_eq!(snapshot.panes[2].status, Status::Idle);
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
        assert_eq!(snapshot.workspaces[0].attention, None);
        for state in ["working", "done", "idle"] {
            let record = program(state, "claude-code");
            model
                .apply_program_status(ProgramStatusUpdate {
                    pane_id: 1,
                    records: vec![record.clone()],
                })
                .unwrap();
            let snapshot = model.snapshot(None, 1);
            assert_eq!(snapshot.workspaces[0].status, record.status());
            assert_eq!(snapshot.workspaces[0].attention, None);
        }
        model
            .apply_program_status(ProgramStatusUpdate {
                pane_id: 1,
                records: vec![],
            })
            .unwrap();
        inspect(&mut model, vec![], 2_000);
        let snapshot = model.snapshot(None, 1);
        assert!(snapshot.panes.iter().all(|pane| pane.harness.is_none()));
        assert_eq!(snapshot.workspaces[0].status, Status::Unknown);
        assert_eq!(snapshot.workspaces[0].attention, None);
    }

    fn recovery_view(panes: &[(u32, Option<&str>)]) -> RecoveryView {
        let leaf = |id: u32, agent: Option<&str>| RecoveryPane {
            cwd: None,
            name: format!("Terminal {id}"),
            renamed: false,
            harness: agent.map(|_| "claude".to_owned()),
            agent_id: agent.map(str::to_owned),
            focused: false,
        };
        RecoveryView {
            workspaces: vec![RecoveryWorkspace {
                name: "Workspace".to_owned(),
                tabs: vec![RecoveryTab {
                    name: "Tab".to_owned(),
                    focused: true,
                    layout: SplitNode::Split {
                        direction: SplitDirection::Vertical,
                        children: panes
                            .iter()
                            .map(|(id, agent)| SplitNode::Pane {
                                pane: leaf(*id, *agent),
                            })
                            .collect(),
                    },
                }],
            }],
            keys: panes
                .iter()
                .map(|(id, agent)| (*id, agent.map(str::to_owned)))
                .collect(),
        }
    }

    const SESSION: (&str, &str) = ("test", "test-session-id");

    fn sent(decision: RecoveryDecision) -> RecoveryUpdate {
        match decision {
            RecoveryDecision::Send(update) => update,
            other => panic!("expected a send, got {other:?}"),
        }
    }

    #[test]
    fn recovery_workspaces_orders_and_names() {
        let mut model = model();
        // Tab IDs, positions and workspace keys deliberately disagree on order.
        let tabs = [tab(10, 0), tab(20, 2), tab(30, 1)];
        let tiled = |id: u32, x: usize| PaneInfo {
            pane_x: x,
            pane_columns: 50,
            pane_rows: 20,
            ..pane(id)
        };
        let panes = manifest(&[
            (
                0,
                vec![
                    PaneInfo {
                        is_plugin: true,
                        ..tiled(1, 0)
                    },
                    tiled(1, 50),
                ],
            ),
            (
                1,
                vec![
                    tiled(3, 0),
                    tiled(4, 50),
                    PaneInfo {
                        is_floating: true,
                        ..tiled(5, 10)
                    },
                    // Suppressed without a visible counterpart (not a stack member).
                    PaneInfo {
                        is_suppressed: true,
                        ..tiled(6, 20)
                    },
                ],
            ),
            (2, vec![tiled(2, 0)]),
        ]);
        model
            .adopt_context(host_context(
                &[
                    (10, "ws-b", "Beta"),
                    (20, "ws-a", "Alpha"),
                    (30, "ws-a", "Alpha"),
                ],
                &[(1, 10), (2, 20), (3, 30), (4, 30), (5, 30), (6, 30)],
            ))
            .unwrap();
        assert!(model.reconcile_topology(&tabs, &panes));
        assert!(model.panes.contains_key(&5) && model.panes.contains_key(&6));
        model.observe_cwd(3, "/repo".to_owned());
        let renamed = model.panes.get_mut(&3).unwrap();
        renamed.name = "Custom".to_owned();
        renamed.renamed = true;
        model.agents.insert(
            3,
            AgentExecution {
                agent_id: Some("agent-3".to_owned()),
                ..execution(3)
            },
        );
        // A recognized but non-resumable harness keeps neither harness nor association.
        model.agents.insert(
            4,
            AgentExecution {
                harness: "aider".to_owned(),
                agent_id: Some("agent-4".to_owned()),
                ..execution(4)
            },
        );
        model.agents.insert(
            2,
            AgentExecution {
                harness: "codex".to_owned(),
                ..execution(2)
            },
        );

        let geometry = recovery_geometry(&tabs, &panes);
        let view = model.recovery_workspaces(&geometry, &tabs, Some(4));
        let leaf =
            |id: u32, harness: Option<&str>, agent: Option<&str>, focused: bool| RecoveryPane {
                cwd: model.panes[&id].cwd.clone(),
                name: model.pane_name(id, &model.panes[&id]),
                renamed: model.panes[&id].renamed,
                harness: harness.map(str::to_owned),
                agent_id: agent.map(str::to_owned),
                focused,
            };
        assert_eq!(
            view.workspaces,
            vec![
                RecoveryWorkspace {
                    name: "Alpha".to_owned(),
                    tabs: vec![
                        RecoveryTab {
                            name: "Tab 30".to_owned(),
                            focused: true,
                            layout: SplitNode::Split {
                                direction: SplitDirection::Vertical,
                                children: vec![
                                    SplitNode::Pane {
                                        pane: leaf(3, Some("claude"), Some("agent-3"), false),
                                    },
                                    SplitNode::Pane {
                                        pane: leaf(4, None, None, true),
                                    },
                                ],
                            },
                        },
                        RecoveryTab {
                            name: "Tab 20".to_owned(),
                            focused: false,
                            layout: SplitNode::Pane {
                                pane: leaf(2, Some("codex"), None, false),
                            },
                        },
                    ],
                },
                RecoveryWorkspace {
                    name: "Beta".to_owned(),
                    tabs: vec![RecoveryTab {
                        name: "Tab 10".to_owned(),
                        focused: false,
                        layout: SplitNode::Pane {
                            pane: leaf(1, None, None, false),
                        },
                    }],
                },
            ]
        );
        assert_eq!(view.workspaces[0].tabs[0].layout.leaves()[0].name, "Custom");
        assert!(view.workspaces[0].tabs[0].layout.leaves()[0].renamed);
        assert_eq!(
            view.workspaces[0].tabs[0].layout.leaves()[0].cwd.as_deref(),
            Some("/repo")
        );
        assert_eq!(
            view.keys,
            BTreeMap::from([
                (1, None),
                (2, None),
                (3, Some("agent-3".to_owned())),
                (4, None)
            ])
        );
    }

    #[test]
    fn additions_send_within_coalesce_window() {
        let mut gate = RecoveryGate::default();
        let t = 50_000;
        let first = sent(gate.offer(recovery_view(&[(1, None)]), t, SESSION));
        assert_eq!(
            (first.session_name.as_str(), first.session_id.as_str()),
            SESSION
        );
        assert_eq!(first.sequence, 1);
        assert!(gate.finish(1, Some(0), b"", 0));
        assert_eq!(
            gate.offer(recovery_view(&[(1, None), (2, None)]), t + 200, SESSION),
            RecoveryDecision::WaitUntil(gate.last_sent + RECOVERY_COALESCE_MILLIS)
        );
        assert_eq!(gate.last_sent + RECOVERY_COALESCE_MILLIS, t + 1_000);
        let second = sent(gate.offer(recovery_view(&[(1, None), (2, None)]), t + 1_000, SESSION));
        assert_eq!(second.sequence, 2);
        assert_eq!(
            second.workspaces,
            recovery_view(&[(1, None), (2, None)]).workspaces
        );
        assert_eq!(gate.pending, Some(second));
    }

    #[test]
    fn recovery_never_sends_an_empty_topology() {
        let mut gate = RecoveryGate::default();
        let empty = RecoveryView {
            workspaces: Vec::new(),
            keys: BTreeMap::new(),
        };
        assert_eq!(
            gate.offer(empty, 1_000, SESSION),
            RecoveryDecision::Unchanged
        );
        assert_eq!(gate.sequence, 0);
    }

    #[test]
    fn removal_waits_for_grace() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 0);
        let t = 20_000;
        let removed = || recovery_view(&[(1, None)]);
        assert_eq!(
            gate.offer(removed(), t, SESSION),
            RecoveryDecision::WaitUntil(t + RECOVERY_REMOVAL_GRACE_MILLIS)
        );
        assert_eq!(
            gate.offer(removed(), t + 2_999, SESSION),
            RecoveryDecision::WaitUntil(t + 3_000)
        );
        let update = sent(gate.offer(removed(), t + 3_000, SESSION));
        assert_eq!(update.workspaces, removed().workspaces);
        // The hold lasts until the removal is written, so a retry never restarts grace.
        assert_eq!(gate.held_since, Some(t));
        assert!(gate.finish(update.sequence, Some(0), b"", t + 3_000));
        assert_eq!(gate.held_since, None);
    }

    #[test]
    fn association_loss_waits_for_grace() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, Some("agent-1"))]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 0);
        let t = 20_000;
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), t, SESSION),
            RecoveryDecision::WaitUntil(t + 3_000)
        );
        // Gaining an association is an addition and only coalesces.
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None)]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 0);
        sent(gate.offer(recovery_view(&[(1, Some("agent-1"))]), t, SESSION));
    }

    #[test]
    fn removal_reverted_within_grace_cancels_hold() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 0);
        let t = 20_000;
        assert!(matches!(
            gate.offer(recovery_view(&[(1, None)]), t, SESSION),
            RecoveryDecision::WaitUntil(_)
        ));
        assert_eq!(gate.held_since, Some(t));
        assert_eq!(
            gate.offer(recovery_view(&[(1, None), (2, None)]), t + 1_000, SESSION),
            RecoveryDecision::Unchanged
        );
        assert_eq!(gate.held_since, None);
        // A later removal starts a fresh grace period.
        assert_eq!(
            gate.offer(recovery_view(&[(2, None)]), t + 2_500, SESSION),
            RecoveryDecision::WaitUntil(t + 5_500)
        );
    }

    #[test]
    fn one_update_in_flight() {
        let mut gate = RecoveryGate::default();
        let t = 10_000;
        sent(gate.offer(recovery_view(&[(1, None)]), t, SESSION));
        assert_eq!(gate.in_flight, Some(1));
        let next = || recovery_view(&[(1, None), (2, None)]);
        assert!(matches!(
            gate.offer(next(), t + 5_000, SESSION),
            RecoveryDecision::WaitUntil(_)
        ));
        assert!(
            !gate.finish(2, Some(0), b"", 0),
            "foreign sequence must not clear it"
        );
        assert_eq!(gate.in_flight, Some(1));
        assert!(gate.finish(1, Some(0), b"", 0));
        assert_eq!(gate.in_flight, None);
        assert_eq!(sent(gate.offer(next(), t + 5_000, SESSION)).sequence, 2);
    }

    #[test]
    fn stale_in_flight_expires_after_the_helper_timeout() {
        let mut gate = RecoveryGate::default();
        let t = 10_000;
        sent(gate.offer(recovery_view(&[(1, None)]), t, SESSION));
        assert_eq!(
            gate.offer(recovery_view(&[(1, None), (2, None)]), t + 1, SESSION),
            RecoveryDecision::WaitUntil(t + RECOVERY_RESULT_MILLIS)
        );
        assert!(!gate.expire(t + RECOVERY_RESULT_MILLIS - 1));
        let lost = t + RECOVERY_RESULT_MILLIS;
        assert!(gate.expire(lost));
        assert_eq!(gate.in_flight, None);
        // A lost result is a failure: the never-acknowledged update is resent after backoff.
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), lost, SESSION),
            RecoveryDecision::WaitUntil(lost + RECOVERY_RETRY_MILLIS)
        );
        let update = sent(gate.offer(
            recovery_view(&[(1, None)]),
            lost + RECOVERY_RETRY_MILLIS,
            SESSION,
        ));
        assert_eq!(update.sequence, 2);
    }

    #[test]
    fn failed_send_is_resent_unchanged_after_backoff() {
        let mut gate = RecoveryGate::default();
        let view = || recovery_view(&[(1, None), (2, None)]);
        let t = 10_000;
        sent(gate.offer(view(), t, SESSION));
        let failed = t + 100;
        assert!(gate.finish(1, Some(1), b"", failed));
        assert!(gate.acknowledged.is_none() && gate.pending.is_none());
        assert_eq!(
            gate.offer(view(), failed + RECOVERY_RETRY_MILLIS - 1, SESSION),
            RecoveryDecision::WaitUntil(failed + RECOVERY_RETRY_MILLIS)
        );
        let retry = sent(gate.offer(view(), failed + RECOVERY_RETRY_MILLIS, SESSION));
        assert_eq!((retry.sequence, retry.workspaces), (2, view().workspaces));
        // Repeated failures double the delay up to the cap; success resets it.
        let mut now = failed + RECOVERY_RETRY_MILLIS;
        let mut delays = Vec::new();
        for sequence in 2..9 {
            assert!(gate.finish(sequence, Some(1), b"", now));
            delays.push(gate.backoff);
            now = gate.retry_at.unwrap();
            sent(gate.offer(view(), now, SESSION));
        }
        assert_eq!(
            delays,
            [2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );
        assert!(gate.finish(9, Some(0), b"", now));
        assert_eq!((gate.backoff, gate.retry_at), (0, None));
        assert_eq!(
            gate.acknowledged.as_ref().map(|update| update.sequence),
            Some(9)
        );
        assert_eq!(
            gate.offer(view(), now + 60_000, SESSION),
            RecoveryDecision::Unchanged
        );
    }

    #[test]
    fn removal_detection_compares_against_the_acknowledged_baseline() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 10_000);
        // An unwritten addition is not a baseline: dropping pane 3 again is no removal.
        sent(gate.offer(
            recovery_view(&[(1, None), (2, None), (3, None)]),
            20_000,
            SESSION,
        ));
        gate.finish(2, Some(1), b"", 20_000);
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), 30_000, SESSION),
            RecoveryDecision::WaitUntil(30_000 + RECOVERY_REMOVAL_GRACE_MILLIS)
        );
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), 10_000, SESSION));
        gate.finish(1, Some(0), b"", 10_000);
        sent(gate.offer(
            recovery_view(&[(1, None), (2, None), (3, None)]),
            20_000,
            SESSION,
        ));
        gate.finish(2, Some(1), b"", 20_000);
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), 30_000, SESSION));
    }

    #[test]
    fn oversized_refusal_backs_off_like_a_failure() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None)]), 10_000, SESSION));
        gate.refuse(10_000);
        assert_eq!(gate.in_flight, None);
        assert_eq!(gate.retry_at, Some(10_000 + RECOVERY_RETRY_MILLIS));
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), 10_500, SESSION),
            RecoveryDecision::WaitUntil(11_000)
        );
    }

    #[test]
    fn owner_settles_recovery_results_into_the_shared_model() {
        let directory = std::env::temp_dir().join(format!(
            "bkpr-native-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut model = model();
        sent(
            model
                .recovery
                .offer(recovery_view(&[(1, None)]), 10_000, SESSION),
        );
        let mut owner = Owner {
            configuration: model.configuration.clone(),
            model: Some(model),
            storage: directory.join("owner.json"),
            granted: true,
            ..Owner::default()
        };
        owner.save();
        let result = |code: i32, stderr: &str, sequence: u64| {
            Event::RunCommandResult(
                Some(code),
                Vec::new(),
                stderr.as_bytes().to_vec(),
                BTreeMap::from([("native_recovery".to_owned(), sequence.to_string())]),
            )
        };
        let gate = |owner: &Owner| owner.model.as_ref().unwrap().recovery.clone();
        let error = |owner: &Owner| owner.model.as_ref().unwrap().recovery_error.clone();

        // Failure surfaces in the sidebar and releases the in-flight sequence.
        owner.event(result(1, "disk full", 1));
        assert_eq!(gate(&owner).in_flight, None);
        assert!(gate(&owner).retry_at.is_some());
        assert!(error(&owner).is_some_and(|error| error.contains("disk full")));
        assert!(
            owner
                .model
                .as_ref()
                .unwrap()
                .snapshot(None, 0)
                .error
                .as_deref()
                == error(&owner).as_deref()
        );

        // A foreign sequence is ignored; success clears the error and acknowledges.
        owner.model.as_mut().unwrap().recovery.retry_at = Some(0);
        sent(owner.model.as_mut().unwrap().recovery.offer(
            recovery_view(&[(1, None)]),
            20_000,
            SESSION,
        ));
        owner.save();
        owner.event(result(0, "", 7));
        assert_eq!(gate(&owner).in_flight, Some(2));
        owner.event(result(0, "", 2));
        assert_eq!(gate(&owner).in_flight, None);
        assert_eq!(error(&owner), None);
        assert_eq!(
            gate(&owner).acknowledged.map(|update| update.sequence),
            Some(2)
        );

        // Exit 3 (unclaimed) disables recovery without a sidebar error.
        sent(owner.model.as_mut().unwrap().recovery.offer(
            recovery_view(&[(1, None), (2, None)]),
            30_000,
            SESSION,
        ));
        owner.save();
        owner.event(result(3, "session recovery is not claimed", 3));
        assert!(gate(&owner).disabled);
        assert_eq!(gate(&owner).in_flight, None);
        assert_eq!(error(&owner), None);
        // The settlement is persisted for other owner replicas.
        let mut replica = Owner {
            configuration: owner.configuration.clone(),
            storage: owner.storage.clone(),
            ..Owner::default()
        };
        replica.recover_model();
        assert!(gate(&replica).disabled);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_sequence_from_a_recreated_owner_jumps_past_the_record() {
        // A recreated model restarts at sequence 1 while the record is at 41.
        let mut gate = RecoveryGate::default();
        let view = || recovery_view(&[(1, None)]);
        let t = 10_000;
        assert_eq!(sent(gate.offer(view(), t, SESSION)).sequence, 1);
        assert!(gate.finish(1, Some(RECOVERY_STALE_EXIT), b"41\n", t + 100));
        assert_eq!(gate.backoff, 0);
        assert!(gate.pending.is_none() && gate.acknowledged.is_none());
        // Resent promptly (only the usual coalescing), past the record's sequence.
        let resend = sent(gate.offer(view(), t + RECOVERY_COALESCE_MILLIS, SESSION));
        assert_eq!(
            (resend.sequence, resend.workspaces),
            (42, view().workspaces)
        );
        assert_eq!(recovery_failure(Some(RECOVERY_STALE_EXIT), b""), None);
        // Even unchanged contents are resent after a stale result.
        assert!(gate.finish(42, Some(0), b"", t + 1_100));
        sent(gate.offer(recovery_view(&[(1, None), (2, None)]), t + 2_100, SESSION));
        assert!(gate.finish(43, Some(RECOVERY_STALE_EXIT), b"50", t + 2_200));
        let resend = sent(gate.offer(view(), t + 3_200, SESSION));
        assert_eq!(resend.sequence, 51);
        // The sequence never moves backwards; unreadable output is an ordinary failure.
        assert!(gate.finish(51, Some(RECOVERY_STALE_EXIT), b"7", t + 3_300));
        assert_eq!(gate.sequence, 51);
        sent(gate.offer(view(), t + 4_300, SESSION));
        assert!(gate.finish(52, Some(RECOVERY_STALE_EXIT), b"x", t + 4_400));
        assert_eq!(gate.backoff, RECOVERY_RETRY_MILLIS);
    }

    #[test]
    fn unclaimed_session_disables_recovery_silently() {
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None)]), 10_000, SESSION));
        assert!(gate.finish(1, Some(3), b"", 0));
        assert!(gate.disabled);
        assert_eq!(
            gate.offer(recovery_view(&[(1, None), (2, None)]), 20_000, SESSION),
            RecoveryDecision::Unchanged
        );
        assert_eq!(recovery_failure(Some(3), b"unclaimed"), None);
        assert_eq!(recovery_failure(Some(0), b""), None);
        assert!(
            recovery_failure(Some(1), b"stale recovery update")
                .is_some_and(|error| error.contains("stale recovery update"))
        );
        assert!(recovery_failure(None, b"").is_some());
    }

    #[test]
    fn oversized_recovery_update_is_not_spawned() {
        let mut update = RecoveryUpdate {
            session_name: "test".to_owned(),
            session_id: "test-session-id".to_owned(),
            sequence: 1,
            workspaces: recovery_view(&[(1, None)]).workspaces,
        };
        assert!(recovery_argument(&update).is_ok());
        update.workspaces[0].name = "x".repeat(RECOVERY_ARGUMENT_BYTES);
        assert!(
            recovery_argument(&update)
                .is_err_and(|error| error.contains("recovery update too large"))
        );
    }

    #[test]
    fn recovery_gate_defaults_for_legacy_models() {
        let mut value = serde_json::to_value(model()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("recovery");
        object.remove("recovery_error");
        object.remove("restore");
        let decoded = Model::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(decoded.recovery.sequence, 0);
        assert!(decoded.recovery_error.is_none());
        assert_eq!(decoded.restore, RestoreNames::default());
        assert_eq!(decoded.recovery.restore_expected, None);
        let error = Model {
            recovery_error: Some("Session recovery update failed: disk full".to_owned()),
            ..model()
        };
        assert_eq!(
            error.snapshot(None, 0).error.as_deref(),
            Some("Session recovery update failed: disk full")
        );
    }

    #[test]
    fn restore_keys_are_stripped_before_identity() {
        let ordinary = BTreeMap::from([
            ("role".to_owned(), "native-owner".to_owned()),
            ("session".to_owned(), "test".to_owned()),
            ("session_id".to_owned(), "test-session-id".to_owned()),
            ("context_owner".to_owned(), "private-test-owner".to_owned()),
            ("program".to_owned(), "/bin/bkpr".to_owned()),
            ("initial_cwd".to_owned(), "/work".to_owned()),
            ("plugin_url".to_owned(), "file:/bkpr.wasm".to_owned()),
            ("sidebar_width".to_owned(), "28".to_owned()),
        ]);
        let mut restored = ordinary.clone();
        restored.insert(
            "restore_names".to_owned(),
            r#"{"w0t0p1":"Build \"server\""}"#.to_owned(),
        );
        restored.insert("restore_expected_panes".to_owned(), "3".to_owned());
        restored.insert(
            "restore_links".to_owned(),
            r#"{"w0t0p2":{"harness":"codex","agent_id":"agent-7"}}"#.to_owned(),
        );
        let seed = take_restore_seed(&mut restored).unwrap();
        assert_eq!(restored, ordinary);
        assert!(configuration_valid(&restored, "native-owner"));
        assert_eq!(
            seed.names,
            BTreeMap::from([("w0t0p1".to_owned(), r#"Build "server""#.to_owned())])
        );
        assert_eq!(seed.expected, Some(3));
        assert_eq!(
            seed.links,
            BTreeMap::from([(
                "w0t0p2".to_owned(),
                RestoreLink {
                    harness: "codex".to_owned(),
                    agent_id: "agent-7".to_owned(),
                }
            )])
        );
        // An ordinary session has none of the keys.
        let mut plain = ordinary.clone();
        assert_eq!(
            take_restore_seed(&mut plain).unwrap(),
            RestoreSeed::default()
        );
        assert_eq!(plain, ordinary);
        // Each malformed key reports its own error, with both keys stripped.
        let mut malformed = ordinary.clone();
        malformed.insert("restore_names".to_owned(), "{".to_owned());
        malformed.insert("restore_expected_panes".to_owned(), "x".to_owned());
        assert!(
            take_restore_seed(&mut malformed).is_err_and(|error| error.contains("restore_names"))
        );
        assert_eq!(malformed, ordinary);
        let mut malformed = ordinary.clone();
        malformed.insert("restore_names".to_owned(), "{}".to_owned());
        malformed.insert("restore_expected_panes".to_owned(), "x".to_owned());
        assert!(
            take_restore_seed(&mut malformed)
                .is_err_and(|error| error.contains("restore_expected_panes"))
        );
        assert_eq!(malformed, ordinary);
        let mut malformed = ordinary.clone();
        malformed.insert("restore_links".to_owned(), r#"{"k":"v"}"#.to_owned());
        assert!(
            take_restore_seed(&mut malformed).is_err_and(|error| error.contains("restore_links"))
        );
        assert_eq!(malformed, ordinary);
        for (key, value) in [
            ("restore_names", "[1]"),
            ("restore_names", "{\"k\":1}"),
            ("restore_expected_panes", "-1"),
            ("restore_expected_panes", "many"),
        ] {
            let mut malformed = ordinary.clone();
            malformed.insert(key.to_owned(), value.to_owned());
            assert!(take_restore_seed(&mut malformed).is_err(), "{key}={value}");
        }
    }

    /// Shaped like the stack probe's `list-panes` data: hidden stack members are
    /// suppressed, non-floating and full-height at the visible member's rectangle, in the
    /// active tab and in inactive ones. Floating and unrelated suppressed panes stay out.
    #[test]
    fn recovery_records_suppressed_stack_members() {
        let mut model = model();
        let tabs = [tab(10, 0), tab(20, 1)];
        let full = |id: u32, suppressed: bool| PaneInfo {
            pane_x: 38,
            pane_y: 0,
            pane_columns: 162,
            pane_rows: 50,
            is_suppressed: suppressed,
            ..pane(id)
        };
        let sidebar = |id: u32| PaneInfo {
            is_plugin: true,
            pane_x: 0,
            pane_columns: 38,
            pane_rows: 50,
            ..pane(id)
        };
        let panes = manifest(&[
            (
                0,
                vec![
                    sidebar(1),
                    full(0, true),
                    full(1, false),
                    full(2, true),
                    PaneInfo {
                        is_floating: true,
                        ..full(3, false)
                    },
                    // A suppressed pane matching no visible rectangle is not a member.
                    PaneInfo {
                        pane_x: 60,
                        pane_columns: 20,
                        ..full(4, true)
                    },
                ],
            ),
            // Inactive tab: the runtime has already folded this stack into one list.
            (
                1,
                vec![sidebar(2), full(5, true), full(6, true), full(7, false)],
            ),
        ]);
        model
            .adopt_context(host_context(
                &[(10, "ws-a", "Alpha"), (20, "ws-a", "Alpha")],
                &[
                    (0, 10),
                    (1, 10),
                    (2, 10),
                    (3, 10),
                    (4, 10),
                    (5, 20),
                    (6, 20),
                    (7, 20),
                ],
            ))
            .unwrap();
        assert!(model.reconcile_topology(&tabs, &panes));
        for (id, pane) in model.panes.iter_mut() {
            pane.name = format!("p{id}");
            pane.renamed = true;
        }
        let view = model.recovery_workspaces(&recovery_geometry(&tabs, &panes), &tabs, Some(1));
        let stacks: Vec<_> = view.workspaces[0]
            .tabs
            .iter()
            .map(|tab| match &tab.layout {
                SplitNode::Stack { panes } => panes
                    .iter()
                    .map(|pane| (pane.name.clone(), pane.focused))
                    .collect::<Vec<_>>(),
                other => panic!("expected a stack, got {other:?}"),
            })
            .collect();
        let name = |id: u32| format!("p{id}");
        assert_eq!(
            stacks,
            vec![
                vec![(name(0), false), (name(1), true), (name(2), false)],
                vec![(name(5), false), (name(6), false), (name(7), false)],
            ]
        );
        assert!(view.workspaces[0].tabs[0].focused);
        assert_eq!(
            view.keys.keys().copied().collect::<Vec<_>>(),
            [0, 1, 2, 5, 6, 7]
        );
    }

    /// The launcher sends `restore_expected_panes = RecoveryRecord::pane_count()`; the owner
    /// compares it with `RecoveryView::keys`. Both must count every recorded terminal leaf,
    /// including stacked panes, or a restored session's first update waits needlessly.
    #[test]
    fn restore_expected_panes_counts_the_owner_unit() {
        use bkpr::recovery_contract::RecoveryRecord;

        let mut model = model();
        let tabs = [tab(10, 0), tab(20, 1)];
        let at = |id: u32, x: usize, y: usize, rows: usize| PaneInfo {
            pane_x: x,
            pane_y: y,
            pane_columns: 50,
            pane_rows: rows,
            ..pane(id)
        };
        let panes = manifest(&[
            (
                0,
                vec![
                    at(1, 0, 0, 20),
                    at(2, 50, 0, 1),
                    at(3, 50, 1, 1),
                    at(4, 50, 2, 18),
                ],
            ),
            (1, vec![at(5, 0, 0, 20)]),
        ]);
        model
            .adopt_context(host_context(
                &[(10, "ws-a", "Alpha"), (20, "ws-b", "Beta")],
                &[(1, 10), (2, 10), (3, 10), (4, 10), (5, 20)],
            ))
            .unwrap();
        assert!(model.reconcile_topology(&tabs, &panes));
        let view = model.recovery_workspaces(&recovery_geometry(&tabs, &panes), &tabs, Some(1));
        let SplitNode::Split { children, .. } = &view.workspaces[0].tabs[0].layout else {
            panic!("expected a split beside the stack");
        };
        assert!(matches!(&children[1], SplitNode::Stack { panes } if panes.len() == 3));
        let record = RecoveryRecord {
            version: bkpr::recovery_contract::RECOVERY_VERSION,
            session_name: "test".to_owned(),
            session_id: "test-session-id".to_owned(),
            sequence: 1,
            written_by: "test".to_owned(),
            updated_at_millis: 0,
            workspaces: view.workspaces.clone(),
        };
        assert_eq!(record.pane_count(), 5);
        assert_eq!(view.keys.len(), record.pane_count());
    }

    #[test]
    fn created_tabs_never_inherit_restore_keys() {
        let raw = BTreeMap::from([
            ("role".to_owned(), "native-owner".to_owned()),
            ("session".to_owned(), "test".to_owned()),
            ("session_id".to_owned(), "test-session-id".to_owned()),
            ("context_owner".to_owned(), "private-test-owner".to_owned()),
            ("program".to_owned(), "/bin/bkpr".to_owned()),
            ("initial_cwd".to_owned(), "/work".to_owned()),
            ("plugin_url".to_owned(), "file:/bkpr.wasm".to_owned()),
            ("sidebar_width".to_owned(), "28".to_owned()),
            ("caller_cwd".to_owned(), "/work".to_owned()),
            ("mode_info_format".to_owned(), "json".to_owned()),
            (
                "restore_names".to_owned(),
                r#"{"w0t0p0":"Build"}"#.to_owned(),
            ),
            ("restore_expected_panes".to_owned(), "1".to_owned()),
            (
                "restore_links".to_owned(),
                r#"{"w0t0p0":{"harness":"claude","agent_id":"a"}}"#.to_owned(),
            ),
        ]);
        let (identity, seed) = owner_identity(&raw);
        assert_eq!(seed.unwrap().expected, Some(1));
        assert!(configuration_valid(&identity, "native-owner"));
        let tab = tab_sidebar_configuration(&identity);
        assert_eq!(tab["role"], "sidebar");
        for key in [
            "restore_names",
            "restore_expected_panes",
            "restore_links",
            "context_owner",
            "caller_cwd",
            "mode_info_format",
        ] {
            assert!(!tab.contains_key(key), "{key}");
        }
    }

    fn link(agent: &str) -> RestoreLink {
        RestoreLink {
            harness: "claude".to_owned(),
            agent_id: agent.to_owned(),
        }
    }

    /// A restored pane whose resume was refused keeps its recorded association (ruling for
    /// fix 3) until an agent is detected in it or it closes; losing it is an ordinary
    /// removal under the grace hold, never an early write.
    #[test]
    fn retained_restore_link_survives_until_agent_or_close() {
        let mut model = model();
        let tabs = [tab(10, 0)];
        let at = |id: u32, x: usize| PaneInfo {
            pane_x: x,
            pane_columns: 50,
            pane_rows: 20,
            ..pane(id)
        };
        let panes = manifest(&[(0, vec![at(1, 0), at(2, 50)])]);
        model
            .adopt_context(host_context(&[(10, "ws-a", "Alpha")], &[(1, 10), (2, 10)]))
            .unwrap();
        assert!(model.reconcile_topology(&tabs, &panes));
        model.restore.links = BTreeMap::from([
            ("w0t0p0".to_owned(), link("agent-1")),
            ("w0t0p1".to_owned(), link("agent-2")),
        ]);
        // Unknown keys and mismatched associations are ignored and stay claimable.
        assert!(!model.restore_link("w9t9p9", 1, &link("agent-1")));
        assert!(!model.restore_link("w0t0p0", 1, &link("agent-x")));
        assert!(model.restore_link("w0t0p0", 1, &link("agent-1")));
        // One-shot per key.
        assert!(!model.restore_link("w0t0p0", 2, &link("agent-1")));
        assert!(model.restore_link("w0t0p1", 2, &link("agent-2")));

        let view = |model: &Model| {
            model.recovery_workspaces(&recovery_geometry(&tabs, &panes), &tabs, Some(1))
        };
        let associations = |view: &RecoveryView| {
            view.workspaces[0].tabs[0]
                .layout
                .leaves()
                .iter()
                .map(|pane| (pane.harness.clone(), pane.agent_id.clone()))
                .collect::<Vec<_>>()
        };
        let linked = |agent: &str| (Some("claude".to_owned()), Some(agent.to_owned()));
        let first = view(&model);
        assert_eq!(associations(&first), [linked("agent-1"), linked("agent-2")]);
        assert_eq!(first.keys[&1], Some("agent-1".to_owned()));

        let mut gate = RecoveryGate::default();
        let t = 50_000;
        sent(gate.offer(first, t, SESSION));
        assert!(gate.finish(1, Some(0), b"", t));

        // Any detected agent ends the link for good, even after that agent leaves.
        model.observe_roots(BTreeMap::from([(1, 101), (2, 102)]));
        inspect(
            &mut model,
            vec![AgentExecution {
                harness: "omp".to_owned(),
                ..execution(1)
            }],
            t,
        );
        assert!(!model.restore.retained.contains_key(&1));
        inspect(&mut model, Vec::new(), t + AGENT_RESULT_MILLIS + 2);
        // Closing a pane drops its link.
        assert!(model.close_pane(2));
        assert!(model.restore.retained.is_empty());

        let mut lost = view(&model);
        assert_eq!(associations(&lost), [(None, None)]);
        // Re-add pane 2's key unchanged so only pane 1's lost link is the removal.
        lost.keys.insert(2, Some("agent-2".to_owned()));
        let now = t + 2_000;
        assert_eq!(
            gate.offer(lost, now, SESSION),
            RecoveryDecision::WaitUntil(now + RECOVERY_REMOVAL_GRACE_MILLIS)
        );
    }

    #[test]
    fn restore_link_pipe_retains_once_and_acknowledges() {
        let directory = std::env::temp_dir().join(format!(
            "bkpr-native-link-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        model.restore.links = BTreeMap::from([("w0t0p0".to_owned(), link("agent-1"))]);
        let mut owner = Owner {
            configuration: model.configuration.clone(),
            model: Some(model),
            storage: directory.join("owner.json"),
            granted: true,
            ..Owner::default()
        };
        owner.save();
        let message = |source: &str, payload: &str| PipeMessage {
            source: PipeSource::Cli(source.to_owned()),
            name: RESTORE_NAME.to_owned(),
            payload: Some(payload.to_owned()),
            args: BTreeMap::new(),
            is_private: false,
        };
        let acknowledged = || ACKNOWLEDGED_CLI.with(|acknowledged| acknowledged.take());
        let retained = |owner: &Owner| owner.model.as_ref().unwrap().restore.retained.clone();
        acknowledged();

        owner.pipe(message(
            "first",
            r#"{"key":"w0t0p0","pane_id":"terminal_1","harness":"claude","agent_id":"agent-1"}"#,
        ));
        assert_eq!(acknowledged(), ["first"]);
        assert_eq!(retained(&owner), BTreeMap::from([(1, link("agent-1"))]));
        // Persisted in the shared model for per-client replicas.
        let saved = Model::decode(&std::fs::read(directory.join("owner.json")).unwrap()).unwrap();
        assert_eq!(
            saved.restore.retained,
            BTreeMap::from([(1, link("agent-1"))])
        );

        owner.pipe(message(
            "again",
            r#"{"key":"w0t0p0","pane_id":"terminal_2","harness":"claude","agent_id":"agent-1"}"#,
        ));
        owner.pipe(message(
            "unknown",
            r#"{"key":"w9t9p9","pane_id":"terminal_2","harness":"claude","agent_id":"agent-1"}"#,
        ));
        assert_eq!(acknowledged(), ["again", "unknown"]);
        assert_eq!(retained(&owner), BTreeMap::from([(1, link("agent-1"))]));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn restore_name_pipe_applies_once() {
        let directory = std::env::temp_dir().join(format!(
            "bkpr-native-restore-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut model = model();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1), pane(2)])]));
        model.restore.names = BTreeMap::from([
            ("w0t0p0".to_owned(), "Sticky one".to_owned()),
            ("w0t0p1".to_owned(), "Sticky two".to_owned()),
        ]);
        let mut owner = Owner {
            configuration: model.configuration.clone(),
            model: Some(model),
            storage: directory.join("owner.json"),
            granted: true,
            ..Owner::default()
        };
        owner.save();
        let message = |source: &str, payload: &str| PipeMessage {
            source: PipeSource::Cli(source.to_owned()),
            name: RESTORE_NAME.to_owned(),
            payload: Some(payload.to_owned()),
            args: BTreeMap::new(),
            is_private: false,
        };
        let acknowledged = || ACKNOWLEDGED_CLI.with(|acknowledged| acknowledged.take());
        let pane = |owner: &Owner, id: u32| owner.model.as_ref().unwrap().panes[&id].clone();
        acknowledged();

        owner.pipe(message(
            "first",
            r#"{"key":"w0t0p0","pane_id":"terminal_1"}"#,
        ));
        assert_eq!(acknowledged(), ["first"]);
        assert_eq!(pane(&owner, 1).name, "Sticky one");
        assert!(pane(&owner, 1).renamed);
        let restore = owner.model.as_ref().unwrap().restore.clone();
        assert!(restore.consumed.contains("w0t0p0"));
        assert!(!restore.names.contains_key("w0t0p0"));

        // A consumed key is ignored, even for another terminal.
        owner.pipe(message(
            "again",
            r#"{"key":"w0t0p0","pane_id":"terminal_2"}"#,
        ));
        assert_eq!(acknowledged(), ["again"]);
        assert!(!pane(&owner, 2).renamed);

        // Unknown keys and malformed payloads are acknowledged and ignored.
        owner.pipe(message(
            "unknown",
            r#"{"key":"w9t9p9","pane_id":"terminal_2"}"#,
        ));
        owner.pipe(message("malformed", "{"));
        owner.pipe(message(
            "bad-pane",
            r#"{"key":"w0t0p1","pane_id":"plugin_2"}"#,
        ));
        assert_eq!(acknowledged(), ["unknown", "malformed", "bad-pane"]);
        assert!(!pane(&owner, 2).renamed);
        assert!(
            owner
                .model
                .as_ref()
                .unwrap()
                .restore
                .names
                .contains_key("w0t0p1")
        );

        // Before the owner is granted, input is queued and acknowledged on replay.
        owner.granted = false;
        owner.pipe(message(
            "early",
            r#"{"key":"w0t0p1","pane_id":"terminal_2"}"#,
        ));
        assert!(acknowledged().is_empty());
        owner.granted = true;
        for queued in std::mem::take(&mut owner.queued) {
            owner.pipe(queued);
        }
        assert_eq!(acknowledged(), ["early"]);
        assert_eq!(pane(&owner, 2).name, "Sticky two");

        // Denied permissions release every queued CLI input.
        owner.granted = false;
        owner.pipe(message(
            "denied",
            r#"{"key":"w0t0p0","pane_id":"terminal_1"}"#,
        ));
        assert!(acknowledged().is_empty());
        owner.event(Event::PermissionRequestResult(PermissionStatus::Denied));
        assert_eq!(acknowledged(), ["denied"]);
        assert!(owner.queued.is_empty());
        owner.error = None;
        owner.granted = true;

        // The claim is persisted for the other owner replicas.
        let mut replica = Owner {
            configuration: owner.configuration.clone(),
            storage: owner.storage.clone(),
            ..Owner::default()
        };
        replica.recover_model();
        assert!(replica.model.unwrap().restore.consumed.contains("w0t0p0"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restore_name_waits_for_its_terminal() {
        let mut model = model();
        model.restore.names = BTreeMap::from([("w0t0p0".to_owned(), "Late".to_owned())]);
        assert!(model.restore_name("w0t0p0", 1));
        assert!(!model.restore_name("w0t0p0", 1));
        assert!(model.apply_restore_names().is_empty());
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![pane(1)])]));
        assert_eq!(model.apply_restore_names(), [(1, "Late".to_owned())]);
        assert_eq!(model.panes[&1].name, "Late");
        assert!(model.panes[&1].renamed);
        assert!(model.restore.pending.is_empty());
        // A later native title does not replace the restored name.
        let mut titled = pane(1);
        titled.title = "vim".to_owned();
        model.reconcile(&[tab(10, 0)], &manifest(&[(0, vec![titled])]));
        assert_eq!(model.panes[&1].name, "Late");
        // A claim for a terminal that closed first is dropped.
        model
            .restore
            .names
            .insert("w0t0p1".to_owned(), "Gone".to_owned());
        model.closed_panes.insert(7);
        assert!(model.restore_name("w0t0p1", 7));
        assert!(model.apply_restore_names().is_empty());
        assert!(model.restore.pending.is_empty());
    }

    #[test]
    fn restore_baseline_waits_for_expected_panes() {
        let mut gate = RecoveryGate {
            restore_expected: Some(3),
            ..RecoveryGate::default()
        };
        let deadline = 1_000 + RESTORE_BASELINE_MILLIS;
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), 1_000, SESSION),
            RecoveryDecision::WaitUntil(deadline)
        );
        assert_eq!(
            gate.offer(recovery_view(&[(1, None), (2, None)]), 2_000, SESSION),
            RecoveryDecision::WaitUntil(deadline)
        );
        // The full restore is recorded as soon as it is present.
        let update = sent(gate.offer(
            recovery_view(&[(1, None), (2, None), (3, None)]),
            3_000,
            SESSION,
        ));
        assert!(gate.finish(update.sequence, Some(0), b"", 3_000));
        assert_eq!(gate.restore_expected, None);
        // Afterwards behavior is unchanged: removals wait out only the ordinary grace.
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), 20_000, SESSION),
            RecoveryDecision::WaitUntil(20_000 + RECOVERY_REMOVAL_GRACE_MILLIS)
        );

        // A restore that never fills up is recorded after the baseline deadline.
        let mut gate = RecoveryGate {
            restore_expected: Some(3),
            ..RecoveryGate::default()
        };
        // An empty topology (before adoption) already starts the clock.
        assert_eq!(
            gate.offer(
                RecoveryView {
                    workspaces: Vec::new(),
                    keys: BTreeMap::new(),
                },
                1_000,
                SESSION
            ),
            RecoveryDecision::Unchanged
        );
        assert_eq!(
            gate.offer(recovery_view(&[(1, None)]), 5_000, SESSION),
            RecoveryDecision::WaitUntil(deadline)
        );
        let update = sent(gate.offer(recovery_view(&[(1, None)]), deadline, SESSION));
        assert_eq!(update.sequence, 1);
        // A failed first write keeps the baseline; its deadline has already passed.
        assert!(gate.finish(1, Some(1), b"", deadline));
        assert_eq!(gate.restore_expected, Some(3));
        let retry = gate.retry_at.unwrap();
        sent(gate.offer(recovery_view(&[(1, None)]), retry, SESSION));

        // Ordinary sessions send their first update immediately.
        let mut gate = RecoveryGate::default();
        sent(gate.offer(recovery_view(&[(1, None)]), 1_000, SESSION));
    }
}
