use crate::backend::{Config, RemoteTarget};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use shephrd::{PROTOCOL_VERSION, Pane, ProjectIdentity, Snapshot, Status, Workspace};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const API_TIMEOUT: Duration = Duration::from_secs(8);
const STRUCTURAL_EVENTS: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.metadata_updated",
    "workspace.renamed",
    "workspace.moved",
    "workspace.reordered",
    "workspace.closed",
    "workspace.focused",
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
    "tab.created",
    "tab.closed",
    "tab.focused",
    "tab.renamed",
    "tab.moved",
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.focused",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    "layout.updated",
];

pub(crate) fn clear_environment(command: &mut Command, prefix: &str) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with(prefix) {
            command.env_remove(key);
        }
    }
}

pub(crate) fn local_command(program: &Path, session: &str, config: Option<&Path>) -> Command {
    let mut command = Command::new(program);
    clear_environment(&mut command, "HERDR_");
    if let Some(config) = config {
        command.env("HERDR_CONFIG_PATH", config);
    }
    command.args(["--session", session]);
    command
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

const CLEAR_REMOTE_ENV: &str = "for variable in $(env | cut -d= -f1); do case \"$variable\" in HERDR_*) unset \"$variable\";; esac; done; ";

pub(crate) fn remote_command(remote: &RemoteTarget, args: &[&str]) -> String {
    let mut script = CLEAR_REMOTE_ENV.to_owned();
    if args == ["remote-api-bridge"] {
        script.push_str("printf '\\n%s\\n' 'shephrd-api-ready:1'; ");
    }
    script.push_str("exec ");
    script.push_str(&shell_quote(&remote.executable));
    for arg in ["--session", remote.session.as_str()]
        .into_iter()
        .chain(args.iter().copied())
    {
        script.push(' ');
        script.push_str(&shell_quote(arg));
    }
    format!("/bin/sh -c {}", shell_quote(&script))
}

pub(crate) fn ssh_args(target: &str, tty: bool) -> Vec<String> {
    vec![
        if tty { "-tt" } else { "-T" }.into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "ServerAliveInterval=5".into(),
        "-o".into(),
        "ServerAliveCountMax=1".into(),
        "--".into(),
        target.into(),
    ]
}

#[derive(Debug, Clone, Deserialize)]
pub struct SavedMachine {
    pub id: String,
    pub label: String,
    pub target: String,
    pub session: String,
    pub enabled: bool,
}

pub fn resolve_machine(
    herdr: &Path,
    selector: &str,
    herdr_config: Option<&Path>,
) -> Result<SavedMachine> {
    let mut command = local_command(herdr, "shephrd", herdr_config);
    command.args(["machine", "list", "--json"]);
    let output = run_output(&mut command, None, API_TIMEOUT)?;
    check_output(&output, "listing saved Herdr machines")?;
    let machines: Vec<SavedMachine> =
        serde_json::from_slice(&output.stdout).context("invalid Herdr machine list")?;
    let exact = machines.iter().find(|machine| machine.id == selector);
    let matches: Vec<_> = machines
        .iter()
        .filter(|machine| machine.label == selector)
        .collect();
    let machine = if let Some(exact) = exact {
        exact
    } else {
        match matches.as_slice() {
            [machine] => machine,
            [] => bail!("saved Herdr machine {selector:?} not found"),
            _ => bail!("saved Herdr machine label {selector:?} is ambiguous; use its ID"),
        }
    };
    if !machine.enabled {
        bail!("saved Herdr machine {:?} is disabled", machine.label);
    }
    Ok(machine.clone())
}

pub fn discover_remote(ssh: &Path, target: &str, session: &str) -> Result<RemoteTarget> {
    validate_target(target)?;
    let script = format!(
        r#"{clear}set -f
emit() {{ [ ! -x "$1" ] || printf '%s\n' "$1"; }}
candidates=$(
command -v herdr
emit "$HOME/.local/bin/herdr"
emit "$HOME/.local/share/mise/installs/herdr/0.9.3/bin/herdr"
emit "$HOME/.local/share/mise/installs/herdr/0.9.3/herdr"
emit "$HOME/.local/share/mise/installs/github-ogulcancelik-herdr/0.9.3/herdr"
emit "$HOME/.nix-profile/bin/herdr"
emit "/etc/profiles/per-user/$USER/bin/herdr"
emit /nix/var/nix/profiles/default/bin/herdr
emit /run/current-system/sw/bin/herdr
emit /home/linuxbrew/.linuxbrew/bin/herdr
emit /opt/homebrew/bin/herdr
emit /usr/local/bin/herdr
)
IFS='
'
for candidate in $candidates; do
    case "$candidate" in */mise/shims/herdr) continue;; /*) ;; *) continue;; esac
    [ -x "$candidate" ] || continue
    if capability=$("$candidate" --session {session} remote-api-bridge --check </dev/null 2>/dev/null) && [ "$capability" = herdr-api-bridge-v1 ]; then
        printf '%s\n' "$candidate"
        exit 0
    fi
done
printf '%s\n' 'No installed POSIX Herdr with public remote-api-bridge-v1 support; install Herdr 0.9.3 on this host.' >&2
exit 2
"#,
        clear = CLEAR_REMOTE_ENV,
        session = shell_quote(session)
    );
    let mut command = Command::new(ssh);
    command
        .args(ssh_args(target, false))
        .arg(format!("/bin/sh -c {}", shell_quote(&script)));
    let output = run_output(&mut command, None, Duration::from_secs(20))?;
    check_output(&output, "discovering remote Herdr executable")?;
    let stdout = std::str::from_utf8(&output.stdout)
        .context("remote discovery returned non-UTF-8 output")?;
    let executable = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with('/'))
        .context("remote discovery did not return an absolute executable path")?;
    Ok(RemoteTarget {
        target: target.into(),
        session: session.into(),
        executable: executable.into(),
    })
}

pub(crate) fn validate_target(target: &str) -> Result<()> {
    if target.is_empty() || target.starts_with('-') || target.contains(['\0', '\n', '\r']) {
        bail!("invalid SSH target");
    }
    Ok(())
}

fn ensure_remote_session(config: &Config, remote: &RemoteTarget) -> Result<()> {
    let mut command = Command::new(&config.ssh);
    command
        .args(ssh_args(&remote.target, false))
        .arg(remote_command(remote, &["remote-client-bridge"]));
    // The stock bridge starts a detached server; EOF disconnects only its transient client.
    let output = run_output(&mut command, None, Duration::from_secs(20))?;
    check_output(&output, "starting selected remote Herdr server")
}

#[derive(Clone)]
struct Endpoint {
    ssh: PathBuf,
    remote: RemoteTarget,
    log: PathBuf,
}

pub(crate) struct Client {
    endpoint: Endpoint,
    sequence: u64,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let remote = &config.remote;
        validate_target(&remote.target)?;
        if !remote.executable.starts_with('/') || remote.executable.contains(['\0', '\n', '\r']) {
            bail!("remote executable must be an absolute POSIX path");
        }
        ensure_remote_session(config, remote)?;
        Ok(Self {
            endpoint: Endpoint {
                ssh: config.ssh.clone(),
                remote: remote.clone(),
                log: config.log_path.clone(),
            },
            sequence: 0,
        })
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = format!("shephrd:{}:{}", std::process::id(), self.sequence);
        let mut connection = self.endpoint.connect()?;
        connection.send(&json!({"id": id, "method": method, "params": params}))?;
        let response = connection.receive(Some(Instant::now() + API_TIMEOUT), None)?;
        response_result(response, &id)
    }

    pub fn snapshot(&mut self) -> Result<RawSnapshot> {
        let mut result = self.request("session.snapshot", json!({}))?;
        if result["type"] != "session_snapshot" {
            bail!("Herdr did not return a session_snapshot");
        }
        serde_json::from_value(
            result
                .get_mut("snapshot")
                .context("Herdr snapshot result is missing")?
                .take(),
        )
        .context("invalid public Herdr snapshot")
    }

    pub fn subscribe(
        &self,
        panes: BTreeSet<String>,
        invalidated: Arc<AtomicBool>,
    ) -> Result<Subscription> {
        let mut connection = self.endpoint.connect()?;
        let mut subscriptions: Vec<Value> = STRUCTURAL_EVENTS
            .iter()
            .map(|event| json!({"type": event}))
            .collect();
        subscriptions.extend(
            panes
                .iter()
                .map(|pane| json!({"type": "pane.agent_status_changed", "pane_id": pane})),
        );
        let id = "shephrd:subscription";
        connection.send(&json!({"id": id, "method": "events.subscribe", "params": {"subscriptions": subscriptions}}))?;
        let result = response_result(
            connection.receive(Some(Instant::now() + API_TIMEOUT), None)?,
            id,
        )?;
        if result["type"] != "subscription_started" {
            bail!("Herdr did not acknowledge event subscription");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let failure = Arc::new(Mutex::new(None));
        let failed = failure.clone();
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match connection.receive(None, Some(&stopped)) {
                    Ok(event) => {
                        if let Some(error) = event.get("error") {
                            *failed.lock() = Some(format!("Herdr event subscription: {error}"));
                            break;
                        }
                        invalidated.store(true, Ordering::Release);
                    }
                    Err(error) => {
                        if !stopped.load(Ordering::Relaxed) {
                            *failed.lock() =
                                Some(format!("Herdr event subscription disconnected: {error:#}"));
                        }
                        break;
                    }
                }
            }
        });
        Ok(Subscription {
            panes,
            failure,
            stop,
            thread: Some(thread),
        })
    }
}

pub(crate) struct Subscription {
    pub panes: BTreeSet<String>,
    pub failure: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Endpoint {
    fn connect(&self) -> Result<Connection> {
        let mut command = Command::new(&self.ssh);
        command
            .args(ssh_args(&self.remote.target, false))
            .arg(remote_command(&self.remote, &["remote-api-bridge"]))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log_file(&self.log)?)
            .process_group(0);
        transient_child(&mut command);
        let mut child = command
            .spawn()
            .context("starting SSH Herdr public API bridge")?;
        let reader = child.stdout.take().unwrap();
        let writer = child.stdin.take().unwrap();
        if let Err(error) =
            nonblocking(reader.as_raw_fd()).and_then(|_| nonblocking(writer.as_raw_fd()))
        {
            terminate(&mut child);
            return Err(error);
        }
        Ok(Connection {
            reader: BufReader::new(reader),
            writer,
            child,
            awaiting_marker: true,
            buffer: Vec::new(),
        })
    }
}

struct Connection {
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
    child: Child,
    awaiting_marker: bool,
    buffer: Vec<u8>,
}

impl Connection {
    fn send(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        if bytes.len() > shephrd::MAX_MESSAGE_BYTES {
            bail!("Herdr request exceeds message limit");
        }
        write_bounded(&mut self.writer, &bytes, Instant::now() + API_TIMEOUT)
    }

    fn receive(&mut self, deadline: Option<Instant>, stop: Option<&AtomicBool>) -> Result<Value> {
        self.buffer.clear();
        let mut banner_bytes = 0;
        loop {
            if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                bail!("subscription stopped");
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("Herdr API response timed out");
            }
            match self.reader.fill_buf() {
                Ok([]) => bail!(
                    "Herdr API connection closed (remote diagnostics are in the companion log)"
                ),
                Ok(buffer) => {
                    let count = buffer
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(buffer.len(), |index| index + 1);
                    if self.buffer.len() + count > shephrd::MAX_MESSAGE_BYTES {
                        bail!("Herdr API message exceeds message limit");
                    }
                    self.buffer.extend_from_slice(&buffer[..count]);
                    self.reader.consume(count);
                    if self.buffer.last() == Some(&b'\n') {
                        if self.awaiting_marker {
                            banner_bytes += self.buffer.len();
                            if banner_bytes > shephrd::MAX_MESSAGE_BYTES {
                                bail!("SSH startup output exceeds message limit");
                            }
                            if self.buffer == b"shephrd-api-ready:1\n" {
                                self.awaiting_marker = false;
                            }
                            self.buffer.clear();
                            continue;
                        }
                        return serde_json::from_slice(&self.buffer)
                            .context("malformed Herdr API JSON");
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    wait_ready(
                        self.reader.get_ref().as_raw_fd(),
                        libc::POLLIN,
                        Duration::from_millis(100),
                    )?;
                }
                Err(error) => return Err(error).context("reading Herdr API"),
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        terminate(&mut self.child);
    }
}

fn response_result(mut response: Value, id: &str) -> Result<Value> {
    if response["id"].as_str() != Some(id) {
        bail!("Herdr response ID does not match request");
    }
    if let Some(error) = response.get("error") {
        return Err(anyhow::Error::new(Rejection(format!(
            "Herdr {}: {}",
            error["code"], error["message"]
        ))));
    }
    response
        .get_mut("result")
        .map(Value::take)
        .context("Herdr response has no result")
}

#[derive(Debug)]
struct Rejection(String);
impl std::fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Rejection {}
pub(crate) fn is_rejection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Rejection>().is_some()
}

fn nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn wait_ready(fd: RawFd, events: libc::c_short, timeout: Duration) -> Result<()> {
    let mut descriptor = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let result = unsafe {
        libc::poll(
            &mut descriptor,
            1,
            timeout.as_millis().min(i32::MAX as u128) as i32,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    Ok(())
}

fn write_bounded(
    writer: &mut (impl Write + AsRawFd),
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            bail!("subprocess/API write timed out");
        }
        match writer.write(bytes) {
            Ok(0) => bail!("subprocess/API input closed"),
            Ok(count) => bytes = &bytes[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                wait_ready(writer.as_raw_fd(), libc::POLLOUT, Duration::from_millis(50))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn log_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("companion log must be a private file owned by the current user");
    }
    Ok(file)
}

fn terminate(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn transient_child(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
}

pub(crate) fn run_output(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    transient_child(command);
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {:?}", command.get_program()))?;
    let outcome = (|| {
        let deadline = Instant::now() + timeout;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut stdin = child.stdin.take();
        nonblocking(stdout.as_raw_fd())?;
        nonblocking(stderr.as_raw_fd())?;
        if let Some(stdin) = &stdin {
            nonblocking(stdin.as_raw_fd())?;
        }
        let mut remaining = input.unwrap_or_default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut scratch = [0u8; 8192];
        loop {
            if Instant::now() >= deadline {
                bail!("subprocess {:?} timed out", command.get_program());
            }
            if let Some(writer) = stdin.as_mut() {
                if remaining.is_empty() {
                    stdin.take();
                } else {
                    match writer.write(remaining) {
                        Ok(0) => bail!("subprocess input closed"),
                        Ok(count) => remaining = &remaining[count..],
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            for (reader, bytes) in [
                (&mut stdout as &mut dyn Read, &mut out),
                (&mut stderr as &mut dyn Read, &mut err),
            ] {
                loop {
                    match reader.read(&mut scratch) {
                        Ok(0) => break,
                        Ok(count) => {
                            if bytes.len() + count > shephrd::MAX_MESSAGE_BYTES {
                                bail!("subprocess output exceeds message limit");
                            }
                            bytes.extend_from_slice(&scratch[..count]);
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) =>
                        {
                            break;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if let Some(status) = child.try_wait()? {
                if !remaining.is_empty() {
                    bail!("subprocess exited before reading its complete input");
                }
                // Pipes were drained above; a final drain handles bytes written just before exit.
                for (reader, bytes) in [
                    (&mut stdout as &mut dyn Read, &mut out),
                    (&mut stderr as &mut dyn Read, &mut err),
                ] {
                    loop {
                        match reader.read(&mut scratch) {
                            Ok(0) => break,
                            Ok(count) => {
                                if bytes.len() + count > shephrd::MAX_MESSAGE_BYTES {
                                    bail!("subprocess output exceeds message limit");
                                }
                                bytes.extend_from_slice(&scratch[..count]);
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::Interrupted
                                ) =>
                            {
                                break;
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if outcome.is_err() {
        terminate(&mut child);
    }
    outcome
}

pub(crate) fn check_output(output: &Output, context: &str) -> Result<()> {
    if !output.status.success() {
        bail!(
            "{context}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSnapshot {
    pub version: String,
    pub protocol: u32,
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<RawWorkspace>,
    pub panes: Vec<RawPane>,
    pub agents: Vec<RawAgent>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawWorkspace {
    pub workspace_id: String,
    pub label: String,
    pub active_tab_id: String,
    pub agent_status: Status,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    pub worktree: Option<RawWorktree>,
}
#[derive(Debug, Deserialize)]
pub(crate) struct RawWorktree {
    pub repo_name: String,
}
#[derive(Debug, Deserialize)]
pub(crate) struct RawPane {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent: Option<String>,
    pub display_agent: Option<String>,
    pub agent_status: Status,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub restore_error: Option<String>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}
#[derive(Debug, Deserialize)]
pub(crate) struct RawAgent {
    pub pane_id: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub display_agent: Option<String>,
    pub agent_status: Status,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

impl RawSnapshot {
    pub fn pane_ids(&self) -> BTreeSet<String> {
        self.panes.iter().map(|pane| pane.pane_id.clone()).collect()
    }
    pub fn workspace_pane(&self, id: &str) -> Option<&str> {
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == id)?;
        self.focused_pane_id
            .as_deref()
            .filter(|focused| {
                self.panes.iter().any(|pane| {
                    pane.pane_id == *focused
                        && pane.workspace_id == id
                        && pane.tab_id == workspace.active_tab_id
                })
            })
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|pane| pane.workspace_id == id && pane.tab_id == workspace.active_tab_id)
                    .map(|pane| pane.pane_id.as_str())
            })
    }
    pub fn present(
        &self,
        config: &Config,
        instance_id: &str,
        revision: u64,
        projects: &mut ProjectCache,
    ) -> Result<Snapshot> {
        if self.protocol == 0 || self.version.is_empty() {
            bail!("invalid Herdr snapshot version/protocol");
        }
        projects.drain();
        let agents: HashMap<_, _> = self
            .agents
            .iter()
            .map(|agent| (agent.pane_id.as_str(), agent))
            .collect();
        let panes: Vec<_> = self
            .panes
            .iter()
            .map(|pane| {
                let agent = agents.get(pane.pane_id.as_str()).copied();
                let status = agent.map_or(pane.agent_status, |agent| agent.agent_status);
                let harness = nonempty(pane.display_agent.as_deref())
                    .or_else(|| agent.and_then(|agent| nonempty(agent.display_agent.as_deref())))
                    .or_else(|| nonempty(pane.agent.as_deref()))
                    .or_else(|| agent.and_then(|agent| nonempty(agent.agent.as_deref())));
                let name = nonempty(pane.label.as_deref())
                    .or_else(|| agent.and_then(|agent| nonempty(agent.name.as_deref())))
                    .or(harness)
                    .unwrap_or("Terminal");
                let labels = agent.map_or(&pane.state_labels, |agent| &agent.state_labels);
                let tokens = agent.map_or(&pane.tokens, |agent| &agent.tokens);
                let description = nonempty(pane.restore_error.as_deref())
                    .or_else(|| {
                        labels
                            .get(status.label())
                            .and_then(|label| nonempty(Some(label)))
                    })
                    .or_else(|| {
                        tokens
                            .get("description")
                            .and_then(|description| nonempty(Some(description)))
                    })
                    .or_else(|| {
                        agent.and_then(|agent| {
                            nonempty(agent.title.as_deref()).and_then(description_title)
                        })
                    })
                    .or_else(|| nonempty(pane.title.as_deref()).and_then(description_title))
                    .or_else(|| nonempty(pane.terminal_title_stripped.as_deref()))
                    .unwrap_or(status.label());
                Pane {
                    id: pane.pane_id.clone(),
                    terminal_id: pane.terminal_id.clone(),
                    workspace_id: pane.workspace_id.clone(),
                    tab_id: pane.tab_id.clone(),
                    name: name.into(),
                    description: description.into(),
                    harness: harness.map(str::to_owned),
                    status,
                    attention: None,
                    cwd: pane.foreground_cwd.as_ref().or(pane.cwd.as_ref()).cloned(),
                    branch: None,
                    branch_changes: None,
                    project: None,
                    ci: None,
                }
            })
            .collect();
        let mut workspaces: Vec<_> = self
            .workspaces
            .iter()
            .map(|workspace| {
                let mut names = Vec::new();
                if let Some(worktree) = &workspace.worktree
                    && !worktree.repo_name.is_empty()
                {
                    names.push(worktree.repo_name.clone());
                }
                for key in ["project", "repo_name"] {
                    if let Some(name) = workspace.tokens.get(key).filter(|name| !name.is_empty()) {
                        names.push(name.clone());
                    }
                }
                Workspace {
                    id: workspace.workspace_id.clone(),
                    name: workspace.label.clone(),
                    active_tab_id: workspace.active_tab_id.clone(),
                    projects: names
                        .into_iter()
                        .map(|name| ProjectIdentity {
                            id: format!(
                                "unknown:{}:{}:{name}",
                                config.remote.target, workspace.workspace_id
                            ),
                            name,
                            repository: false,
                        })
                        .collect(),
                    status: workspace.agent_status,
                    attention: None,
                }
            })
            .collect();
        let workspace_indices: HashMap<_, _> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                (
                    workspace.workspace_id.as_str(),
                    (index, workspace.worktree.is_some()),
                )
            })
            .collect();
        for pane in &panes {
            if let Some((index, has_worktree)) = workspace_indices.get(pane.workspace_id.as_str()) {
                let workspace = &mut workspaces[*index];
                if pane.status.priority() > workspace.status.priority() {
                    workspace.status = pane.status;
                }
                if let Some(cwd) = &pane.cwd
                    && let Some(name) = projects.name(cwd)
                    && (name.repository || !*has_worktree)
                {
                    workspace
                        .projects
                        .retain(|project| project.name != name.name);
                    workspace.projects.push(ProjectIdentity {
                        id: format!("remote:{}:{cwd}", config.remote.target),
                        name: name.name.clone(),
                        repository: false,
                    });
                }
            }
        }
        for workspace in &mut workspaces {
            workspace.projects.sort();
            workspace.projects.dedup();
        }
        Ok(Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
            revision,
            session: config.session.clone(),
            machine: config.machine.clone(),
            connected: true,
            error: None,
            focused_pane_id: self.focused_pane_id.clone(),
            workspaces,
            panes,
        })
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn description_title(title: &str) -> Option<&str> {
    // Herdr strips activity frames from OSC titles, but not producer metadata titles.
    let title = title.trim();
    let first = title.chars().next()?;
    let rest = &title[first.len_utf8()..];
    let activity = matches!(first, '\u{2800}'..='\u{28ff}') || "·✢✳✶✻✽◐◓◑◒".contains(first);
    let title = if activity && (rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        rest.trim()
    } else {
        title
    };
    (!title.is_empty()).then_some(title)
}

use crate::project::{ProjectName, directory_name, repository_name};

pub(crate) struct ProjectCache {
    names: HashMap<String, Option<ProjectName>>,
    pending: BTreeSet<String>,
    requests: mpsc::SyncSender<String>,
    results: mpsc::Receiver<(String, Option<ProjectName>)>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProjectCache {
    pub fn new(config: &Config, invalidated: Arc<AtomicBool>) -> Self {
        let (requests, queue) = mpsc::sync_channel::<String>(32);
        let (completed, results) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let remote = config.remote.clone();
        let ssh = config.ssh.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let cwd = match queue.recv_timeout(Duration::from_millis(100)) {
                    Ok(cwd) => cwd,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let name = discover_project(&cwd, &remote, &ssh);
                if completed.send((cwd, name)).is_err() {
                    break;
                }
                invalidated.store(true, Ordering::Release);
            }
        });
        Self {
            names: HashMap::new(),
            pending: BTreeSet::new(),
            requests,
            results,
            stop,
            worker: Some(worker),
        }
    }

    fn drain(&mut self) {
        while let Ok((cwd, name)) = self.results.try_recv() {
            self.pending.remove(&cwd);
            self.names.insert(cwd, name);
        }
    }

    fn name(&mut self, cwd: &str) -> Option<&ProjectName> {
        if !self.names.contains_key(cwd)
            && !self.pending.contains(cwd)
            && self.requests.try_send(cwd.to_owned()).is_ok()
        {
            self.pending.insert(cwd.to_owned());
        }
        self.names.get(cwd).and_then(Option::as_ref)
    }
}

impl Drop for ProjectCache {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn discover_project(cwd: &str, remote: &RemoteTarget, ssh: &Path) -> Option<ProjectName> {
    let git_args = [
        "-C",
        cwd,
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-common-dir",
    ];
    let mut command = Command::new(ssh);
    let script = format!(
        "exec git {}",
        git_args
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    command
        .args(ssh_args(&remote.target, false))
        .arg(format!("/bin/sh -c {}", shell_quote(&script)));
    let discovered = run_output(&mut command, None, Duration::from_secs(2))
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| repository_name(&output.stdout))
        .map(|name| ProjectName {
            name,
            repository: true,
            branch: None,
            branch_changes: None,
            repository_id: None,
            ci: None,
            source: None,
        });
    discovered.or_else(|| directory_name(Path::new(cwd)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_drop_activity_frames_without_losing_task_text() {
        assert_eq!(description_title(" ⠋ 修复🙂标题 "), Some("修复🙂标题"));
        assert_eq!(description_title("✳ Fix parser"), Some("Fix parser"));
        assert_eq!(description_title("◐   "), None);
        assert_eq!(description_title("⠋task"), Some("⠋task"));
        assert_eq!(description_title("★ production"), Some("★ production"));
        assert_eq!(description_title("task ⠋ detail"), Some("task ⠋ detail"));
    }

    #[test]
    fn linked_worktree_projects_use_repository_not_branch_directory() {
        assert_eq!(
            repository_name(b"/projects/repo/.worktrees/feature-branch\n/projects/repo/.git\n")
                .as_deref(),
            Some("repo")
        );
        assert_eq!(
            repository_name(
                b"ssh banner\n/projects/repo/modules/widget\n/projects/repo/.git/modules/widget\n"
            )
            .as_deref(),
            Some("widget")
        );
        assert!(repository_name(b"relative/project\nrelative/.git\n").is_none());
    }
}
