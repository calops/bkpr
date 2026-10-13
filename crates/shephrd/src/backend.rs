use crate::{herdr, transport};
use anyhow::{Context, Result, bail};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shephrd::{Action, CommandSpec, Effect, PROTOCOL_VERSION, Request, Response, Snapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTarget {
    pub target: String,
    pub session: String,
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub socket: PathBuf,
    pub session: String,
    pub machine: Option<String>,
    pub remote: RemoteTarget,
    pub ssh: PathBuf,
    pub zellij: PathBuf,
    pub zellij_session: String,
    pub plugin_url: String,
    pub plugin_configuration: BTreeMap<String, String>,
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub log_path: PathBuf,
}

impl Config {
    fn validate(&self) -> Result<()> {
        validate_session(&self.session)?;
        herdr::validate_target(&self.remote.target)?;
        validate_session(&self.remote.session)?;
        if self.remote.session != self.session {
            bail!("remote and companion sessions must match");
        }
        for (name, path) in [
            ("socket", &self.socket),
            ("ssh", &self.ssh),
            ("zellij", &self.zellij),
            ("program", &self.program),
            ("cwd", &self.cwd),
            ("log_path", &self.log_path),
        ] {
            if !path.is_absolute() || path.to_str().is_none() {
                bail!("{name} must be an absolute UTF-8 path");
            }
        }
        if self.zellij_session.is_empty() || self.zellij_session.contains(['\0', '\n', '\r']) {
            bail!("invalid Zellij session name");
        }
        if !self.plugin_url.starts_with("file:") {
            bail!("sidebar plugin must use its explicit file URL");
        }
        let expected: BTreeMap<String, String> = BTreeMap::from([
            ("socket".into(), self.socket.to_str().unwrap().into()),
            ("program".into(), self.program.to_str().unwrap().into()),
            ("plugin_url".into(), self.plugin_url.clone()),
            ("role".into(), "sidebar".into()),
        ]);
        if self.plugin_configuration != expected {
            bail!("sidebar plugin configuration does not match the companion target");
        }
        if self.plugin_configuration.iter().any(|(key, value)| {
            key.contains([',', '=', '\0', '\n', '\r']) || value.contains([',', '\0', '\n', '\r'])
        }) {
            bail!(
                "sidebar plugin configuration cannot contain commas or control characters in Zellij's CLI encoding"
            );
        }
        Ok(())
    }
}

pub(crate) fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.len() > 64
        || session == "."
        || session == ".."
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!(
            "Herdr session names must be 1–64 ASCII letters, numbers, '.', '_' or '-' (not '.' or '..')"
        );
    }
    Ok(())
}

pub fn run(config: Config) -> Result<()> {
    config.validate()?;
    let listener = transport::Listener::bind(&config.socket)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let term = signal_hook::flag::register(signal_hook::consts::SIGTERM, stopped.clone())?;
    let interrupt = signal_hook::flag::register(signal_hook::consts::SIGINT, stopped.clone())?;
    let result = run_loop(config, listener, stopped);
    signal_hook::low_level::unregister(term);
    signal_hook::low_level::unregister(interrupt);
    result
}

fn run_loop(config: Config, listener: transport::Listener, stopped: Arc<AtomicBool>) -> Result<()> {
    let instance_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let publisher = Publisher::new(config.clone(), stopped.clone());
    let invalidated = Arc::new(AtomicBool::new(true));
    let projects = herdr::ProjectCache::new(&config, invalidated.clone());
    let mut backend = Backend {
        state: Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id,
            revision: 0,
            session: config.session.clone(),
            machine: config.machine.clone(),
            connected: false,
            error: Some("Connecting to Herdr".into()),
            focused_pane_id: None,
            workspaces: Vec::new(),
            panes: Vec::new(),
        },
        config,
        client: None,
        subscription: None,
        raw: None,
        invalidated,
        projects,
        mappings: BTreeMap::new(),
        pending: BTreeMap::new(),
        publisher,
    };
    let mut reconnect_at = Instant::now();
    let mut refresh_at = Instant::now();
    while !stopped.load(Ordering::Relaxed) {
        let failure = backend
            .subscription
            .as_ref()
            .and_then(|subscription| subscription.failure.lock().take());
        if let Some(error) = failure {
            backend.disconnect(error);
            reconnect_at = Instant::now() + Duration::from_millis(500);
        }
        if backend.subscription.is_none()
            && Instant::now() >= reconnect_at
            && let Err(error) = backend.connect()
        {
            backend.disconnect(format!("{error:#}"));
            reconnect_at = Instant::now() + Duration::from_secs(1);
        }
        if backend.subscription.is_some()
            && Instant::now() >= refresh_at
            && backend.invalidated.swap(false, Ordering::AcqRel)
        {
            if let Err(error) = backend.refresh() {
                backend.disconnect(format!("{error:#}"));
                reconnect_at = Instant::now() + Duration::from_millis(500);
            }
            refresh_at = Instant::now() + Duration::from_millis(80);
        }
        // Work is serialized here; the independent event reader never waits for actions or publication.
        for _ in 0..8 {
            let (mut stream, _) = match listener.socket.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("accepting companion request"),
            };
            if let Err(error) = transport::check_peer(&stream) {
                log_error(&backend.config, &format!("rejected socket peer: {error:#}"));
                continue;
            }
            stream.set_read_timeout(Some(transport::CLIENT_READ_TIMEOUT))?;
            stream.set_write_timeout(Some(transport::CLIENT_READ_TIMEOUT))?;
            let request: Result<Request> =
                transport::read_json(&mut BufReader::new(stream.try_clone()?));
            let response = match request {
                Ok(request) => backend.handle(request),
                Err(error) => Response {
                    protocol: PROTOCOL_VERSION,
                    request_id: 0,
                    state: None,
                    effects: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            };
            if let Err(error) = transport::write_json(&mut stream, &response) {
                log_error(
                    &backend.config,
                    &format!("writing companion response: {error:#}"),
                );
            }
            if stopped.load(Ordering::Relaxed) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

struct Backend {
    config: Config,
    state: Snapshot,
    raw: Option<herdr::RawSnapshot>,
    client: Option<herdr::Client>,
    subscription: Option<herdr::Subscription>,
    invalidated: Arc<AtomicBool>,
    projects: herdr::ProjectCache,
    mappings: BTreeMap<String, u32>,
    pending: BTreeMap<String, Instant>,
    publisher: Publisher,
}

impl Backend {
    fn connect(&mut self) -> Result<()> {
        // Subscribe and drain before issuing the separate snapshot request. There is no recovery cursor.
        let client = herdr::Client::new(&self.config)?;
        let subscription = client.subscribe(BTreeSet::new(), self.invalidated.clone())?;
        self.client = Some(client);
        self.subscription = Some(subscription);
        self.refresh()?;
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        let result = self.refresh_state();
        if let Err(error) = &result {
            self.disconnect(format!("{error:#}"));
        }
        result
    }

    fn refresh_state(&mut self) -> Result<()> {
        let client = self.client.as_mut().context("Herdr is disconnected")?;
        let mut raw = client.snapshot()?;
        let ids = raw.pane_ids();
        if self
            .subscription
            .as_ref()
            .is_none_or(|subscription| subscription.panes != ids)
        {
            // Per-pane status subscriptions are public and parameterized. Swap only after the new
            // subscription is acknowledged and draining; then resnapshot its setup window.
            let subscription = client.subscribe(ids, self.invalidated.clone())?;
            self.subscription.replace(subscription);
            raw = client.snapshot()?;
            if self.subscription.as_ref().unwrap().panes != raw.pane_ids() {
                self.invalidated.store(true, Ordering::Release);
            }
        }
        let mut presented = raw.present(
            &self.config,
            &self.state.instance_id,
            self.state.revision,
            &mut self.projects,
        )?;
        self.mappings.retain(|pane, _| {
            presented
                .panes
                .iter()
                .any(|presented| &presented.id == pane)
        });
        self.pending.retain(|pane, opened| {
            opened.elapsed() < Duration::from_secs(30)
                && presented
                    .panes
                    .iter()
                    .any(|presented| &presented.id == pane)
        });
        self.raw = Some(raw);
        if self.state != presented {
            presented.revision = self.state.revision + 1;
            self.state = presented;
            self.publisher.update(&self.state);
        }
        Ok(())
    }

    fn disconnect(&mut self, error: String) {
        self.subscription.take();
        self.client.take();
        self.pending.clear();
        if self.state.connected || self.state.error.as_deref() != Some(&error) {
            self.state.connected = false;
            self.state.error = Some(error.clone());
            self.state.revision += 1;
            self.publisher.update(&self.state);
            log_error(&self.config, &error);
        }
    }

    fn handle(&mut self, request: Request) -> Response {
        let result = if request.protocol != PROTOCOL_VERSION {
            Err(anyhow::anyhow!(
                "unsupported companion protocol {}; expected {}",
                request.protocol,
                PROTOCOL_VERSION
            ))
        } else {
            self.action(request.action)
        };
        let (effects, error) = match result {
            Ok(effects) => (effects, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        Response {
            protocol: PROTOCOL_VERSION,
            request_id: request.request_id,
            state: Some(self.state.clone()),
            effects,
            error,
        }
    }

    fn action(&mut self, action: Action) -> Result<Vec<Effect>> {
        match action {
            Action::Snapshot => return Ok(Vec::new()),
            Action::RegisterFrontend => {
                self.publisher.register(&self.state);
                return Ok(Vec::new());
            }
            Action::RegisterPane {
                pane_id,
                zellij_pane_id,
            } => {
                self.pane(&pane_id)?;
                self.mappings.retain(|_, mapped| *mapped != zellij_pane_id);
                self.mappings.insert(pane_id.clone(), zellij_pane_id);
                self.pending.remove(&pane_id);
                return Ok(Vec::new());
            }
            Action::PaneClosed { zellij_pane_id } => {
                // Closing a native composition pane detaches only; no destructive Herdr call.
                self.mappings.retain(|_, mapped| *mapped != zellij_pane_id);
                return Ok(Vec::new());
            }
            _ => {}
        }
        if !self.state.connected {
            bail!(
                "Herdr is disconnected: {}",
                self.state.error.as_deref().unwrap_or("unavailable")
            );
        }
        match action {
            Action::SelectPane { pane_id } => {
                self.pane(&pane_id)?;
                self.call("pane.focus", json!({"pane_id": pane_id}))?;
                self.refresh()?;
                self.focus_effect(pane_id)
            }
            Action::FocusPane { pane_id } => {
                self.pane(&pane_id)?;
                self.call("pane.focus", json!({"pane_id": pane_id}))?;
                self.refresh()?;
                // Native focus reports must not emit another native focus (feedback loop).
                Ok(Vec::new())
            }
            Action::SelectWorkspace { workspace_id } => {
                self.call("workspace.focus", json!({"workspace_id": workspace_id}))?;
                self.refresh()?;
                let pane_id = self
                    .raw
                    .as_ref()
                    .and_then(|raw| raw.workspace_pane(&workspace_id))
                    .context("selected workspace has no native terminal in its active tab")?
                    .to_owned();
                self.focus_effect(pane_id)
            }
            Action::CreateWorkspace { label, cwd } => {
                self.call(
                    "workspace.create",
                    json!({"label": label, "cwd": cwd, "focus": true}),
                )?;
                self.refresh()?;
                let pane_id = self
                    .state
                    .focused_pane_id
                    .clone()
                    .context("new workspace has no focused terminal")?;
                self.focus_effect(pane_id)
            }
            Action::CreatePane { workspace_id, cwd } => {
                self.call(
                    "tab.create",
                    json!({"workspace_id": workspace_id, "cwd": cwd, "focus": true}),
                )?;
                self.refresh()?;
                let pane_id = self
                    .state
                    .focused_pane_id
                    .clone()
                    .context("new terminal has no focused pane")?;
                self.focus_effect(pane_id)
            }
            Action::RenameWorkspace {
                workspace_id,
                label,
            } => {
                self.call(
                    "workspace.rename",
                    json!({"workspace_id": workspace_id, "label": label}),
                )?;
                self.refresh()?;
                Ok(Vec::new())
            }
            Action::CloseWorkspace { workspace_id } => {
                self.call(
                    "workspace.close",
                    json!({"workspace_id": workspace_id, "close_group": false}),
                )?;
                self.refresh()?;
                Ok(Vec::new())
            }
            Action::RenamePane { pane_id, label } => {
                self.call("pane.rename", json!({"pane_id": pane_id, "label": label}))?;
                self.refresh()?;
                Ok(Vec::new())
            }
            Action::ClosePane { pane_id } => {
                self.call("pane.close", json!({"pane_id": pane_id}))?;
                self.refresh()?;
                Ok(Vec::new())
            }
            Action::ResolveAttachment { pane_id } => {
                self.refresh()?;
                let pane = self.pane(&pane_id)?;
                let remote = &self.config.remote;
                let mut args = herdr::ssh_args(&remote.target, true);
                args.push(herdr::remote_command(
                    remote,
                    &["terminal", "attach", &pane.terminal_id],
                ));
                let command = CommandSpec {
                    executable: self.config.ssh.to_str().unwrap().into(),
                    args,
                    cwd: Some(self.config.cwd.to_str().unwrap().into()),
                    env: BTreeMap::new(),
                };
                Ok(vec![Effect::Attachment { command }])
            }
            Action::Snapshot
            | Action::RegisterFrontend
            | Action::RegisterPane { .. }
            | Action::PaneClosed { .. } => unreachable!(),
        }
    }

    fn pane(&self, id: &str) -> Result<&shephrd::Pane> {
        self.state
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .with_context(|| format!("Herdr pane {id:?} does not exist"))
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        let result = self
            .client
            .as_mut()
            .context("Herdr is disconnected")?
            .request(method, params);
        if let Err(error) = &result
            && !herdr::is_rejection(error)
        {
            self.disconnect(format!("{error:#}"));
        }
        result.map(|_| ())
    }

    fn focus_effect(&mut self, pane_id: String) -> Result<Vec<Effect>> {
        self.pane(&pane_id)?;
        if let Some(zellij_pane_id) = self.mappings.get(&pane_id) {
            return Ok(vec![Effect::FocusPane {
                zellij_pane_id: *zellij_pane_id,
            }]);
        }
        if self
            .pending
            .get(&pane_id)
            .is_some_and(|opened| opened.elapsed() < Duration::from_secs(30))
        {
            return Ok(Vec::new());
        }
        self.pending.insert(pane_id.clone(), Instant::now());
        Ok(vec![Effect::OpenPane {
            pane_id: pane_id.clone(),
            command: CommandSpec {
                executable: self.config.program.to_str().unwrap().into(),
                args: vec![
                    "attach".into(),
                    "--socket".into(),
                    self.config.socket.to_str().unwrap().into(),
                    "--pane".into(),
                    pane_id,
                ],
                cwd: Some(self.config.cwd.to_str().unwrap().into()),
                env: BTreeMap::new(),
            },
        }])
    }
}

#[derive(Default)]
struct Publication {
    registered: bool,
    latest: Option<Snapshot>,
    stopped: bool,
}

struct Publisher {
    shared: Arc<(Mutex<Publication>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Publisher {
    fn new(config: Config, stopped: Arc<AtomicBool>) -> Self {
        let shared = Arc::new((Mutex::new(Publication::default()), Condvar::new()));
        let worker = shared.clone();
        let thread = thread::spawn(move || {
            let mut seen_alive = false;
            let mut absent = 0;
            let mut monitor_at = Instant::now();
            loop {
                let state = {
                    let (lock, ready) = &*worker;
                    let mut publication = lock.lock();
                    if publication.stopped || stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    if !publication.registered || publication.latest.is_none() {
                        ready.wait_for(&mut publication, Duration::from_millis(200));
                    }
                    if publication.stopped || stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    if publication.registered {
                        publication.latest.take()
                    } else {
                        None
                    }
                };
                if Instant::now() >= monitor_at {
                    match zellij_alive(&config) {
                        Ok(true) => {
                            seen_alive = true;
                            absent = 0;
                        }
                        Ok(false) if seen_alive => {
                            absent += 1;
                            if absent >= 2 {
                                stopped.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            log_error(&config, &format!("monitoring Zellij server: {error:#}"))
                        }
                    }
                    monitor_at = Instant::now() + Duration::from_secs(1);
                }
                if let Some(state) = state
                    && let Err(error) = publish(&config, &state)
                {
                    log_error(&config, &format!("publishing sidebar state: {error:#}"));
                    // Retry the latest authoritative state, but never overwrite a newer one.
                    let mut publication = worker.0.lock();
                    if publication.latest.is_none() {
                        publication.latest = Some(state);
                    }
                    drop(publication);
                    thread::sleep(Duration::from_millis(200));
                }
            }
        });
        Self {
            shared,
            thread: Some(thread),
        }
    }

    fn update(&self, state: &Snapshot) {
        let mut publication = self.shared.0.lock();
        publication.latest = Some(state.clone());
        self.shared.1.notify_one();
    }

    fn register(&self, state: &Snapshot) {
        let mut publication = self.shared.0.lock();
        publication.registered = true;
        publication.latest = Some(state.clone());
        self.shared.1.notify_one();
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.shared.0.lock().stopped = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn zellij_command(config: &Config) -> Command {
    let mut command = Command::new(&config.zellij);
    herdr::clear_environment(&mut command, "ZELLIJ_");
    let directory = config.socket.parent().unwrap().join("zellij");
    command
        .env("ZELLIJ_CONFIG_DIR", &directory)
        .arg("--config-dir")
        .arg(directory);
    command
}

fn zellij_alive(config: &Config) -> Result<bool> {
    let mut command = zellij_command(config);
    // --short loses the EXITED flag and would mistake a resurrection record for a live server.
    command.args(["list-sessions", "--no-formatting"]);
    let output = herdr::run_output(&mut command, None, Duration::from_secs(2))?;
    if output.status.success() {
        let prefix = format!("{} [Created ", config.zellij_session);
        return Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with(&prefix) && !line.contains("(EXITED")));
    }
    if String::from_utf8_lossy(&output.stderr).trim() == "No active zellij sessions found." {
        return Ok(false);
    }
    herdr::check_output(&output, "listing Zellij sessions")?;
    unreachable!()
}

fn publish(config: &Config, state: &Snapshot) -> Result<()> {
    let configuration = config
        .plugin_configuration
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut command = zellij_command(config);
    command
        .arg("--session")
        .arg(&config.zellij_session)
        .args(["pipe", "--plugin"])
        .arg(&config.plugin_url)
        .arg("--plugin-configuration")
        .arg(configuration)
        .args(["--name", "shephrd-state"]);
    let mut bytes = serde_json::to_vec(state)?;
    bytes.push(b'\n');
    if bytes.len() > shephrd::MAX_MESSAGE_BYTES {
        bail!("sidebar snapshot exceeds message limit");
    }
    let output = herdr::run_output(&mut command, Some(&bytes), Duration::from_secs(2))?;
    herdr::check_output(&output, "sending targeted sidebar state")
}

fn log_error(config: &Config, error: &str) {
    if let Ok(mut log) = herdr::log_file(&config.log_path) {
        let _ = writeln!(log, "shephrd: {error}");
    }
}
