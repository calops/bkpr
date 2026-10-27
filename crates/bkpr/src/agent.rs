use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bkpr::MAX_MESSAGE_BYTES;
use bkpr::agent_contract::{AgentExecution, InspectRequest, InspectResponse};

const MAX_PANES: usize = 1024;
const MAX_PROCESSES: usize = 32768;
const MAX_ARGUMENT_BYTES: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Process {
    pid: i32,
    parent: i32,
    group: i32,
    foreground: i32,
    tty: u64,
    stopped: bool,
    identity: String,
}

#[cfg(target_os = "linux")]
fn bounded_read(mut reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("agent message exceeds {limit} bytes");
    }
    Ok(bytes)
}

fn executable_name(executable: &Path) -> Option<&str> {
    let name = executable.file_name()?.to_str()?;
    Some(
        name.strip_prefix('.')
            .and_then(|name| name.strip_suffix("-wrapped"))
            .unwrap_or(name),
    )
}

fn is_python_runtime(name: &str) -> bool {
    matches!(name, "python" | "python3")
        || name.strip_prefix("python3.").is_some_and(|minor| {
            !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn js_entrypoint(arguments: &[OsString]) -> Option<&str> {
    let mut arguments = arguments.iter().skip(1);
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str()?;
        match argument {
            "--" => return arguments.next()?.to_str(),
            "--use-system-ca"
            | "--no-warnings"
            | "--no-deprecation"
            | "--enable-source-maps"
            | "--experimental-strip-types"
            | "--inspect"
            | "--inspect-brk" => {}
            "--require"
            | "-r"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "--conditions"
            | "-C"
            | "--title"
            | "--env-file"
            | "--env-file-if-exists" => {
                arguments.next()?;
            }
            option if option.starts_with("--eval=") || option.starts_with("--print=") => {
                return None;
            }
            option if option.starts_with("--") && option.contains('=') => {}
            option if option.starts_with('-') => return None,
            script => return Some(script),
        }
    }
    None
}

fn recognize_js(script: &str) -> Option<&'static str> {
    if script.ends_with("/@anthropic-ai/claude-code/cli.js") {
        Some("claude")
    } else if script.ends_with("/@github/copilot/npm-loader.js") {
        Some("copilot")
    } else if script.ends_with("/cline/bin/cline") {
        Some("cline")
    } else if script.ends_with("/share/cursor-agent/index.js")
        || (Path::new(script)
            .file_name()
            .is_some_and(|name| name == "index.js")
            && Path::new(script)
                .parent()
                .and_then(Path::parent)
                .is_some_and(|parent| {
                    parent.file_name().is_some_and(|name| name == "versions")
                        && parent
                            .parent()
                            .and_then(Path::file_name)
                            .is_some_and(|name| name == "cursor-agent")
                }))
    {
        Some("cursor")
    } else {
        None
    }
}

fn recognize_python(arguments: &[OsString]) -> Option<&'static str> {
    let mut arguments = arguments.iter().skip(1);
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str()?;
        match argument {
            "-m" => {
                return matches!(arguments.next()?.to_str()?, "aider" | "aider.main")
                    .then_some("aider");
            }
            "-u" | "-B" | "-E" | "-I" | "-s" | "-S" | "-P" => {}
            "-X" | "-W" => {
                arguments.next()?;
            }
            option if option.starts_with("-X") || option.starts_with("-W") => {}
            option if option.starts_with('-') && option != "--" => return None,
            script => {
                let script = if script == "--" {
                    arguments.next()?.to_str()?
                } else {
                    script
                };
                return Path::new(script)
                    .file_name()
                    .is_some_and(|name| name == "aider" || name == ".aider-wrapped")
                    .then_some("aider");
            }
        }
    }
    None
}

fn recognize(executable: &Path, arguments: &[OsString]) -> Option<&'static str> {
    let name = executable_name(executable)?;
    match name {
        "claude" | "claude-code" | "claude.exe" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "omp" => Some("omp"),
        "agy" | "antigravity" => Some("agy"),
        "cursor-agent" => Some("cursor"),
        "copilot" => Some("copilot"),
        "aider" => Some("aider"),
        "cline" | ".cline" => Some("cline"),
        "goose" => Some("goose"),
        "node" | "nodejs" | "bun" => recognize_js(js_entrypoint(arguments)?),
        runtime if is_python_runtime(runtime) => recognize_python(arguments),
        _ => None,
    }
}

fn recognize_process(pid: i32) -> Result<Option<&'static str>> {
    let executable = platform::executable(pid)?;
    let arguments = if executable_name(&executable)
        .is_some_and(|name| matches!(name, "node" | "nodejs" | "bun") || is_python_runtime(name))
    {
        platform::arguments(pid)?
    } else {
        Vec::new()
    };
    Ok(recognize(&executable, &arguments))
}

fn ancestry_depth(
    process: &Process,
    root: &Process,
    processes: &HashMap<i32, Process>,
) -> Option<usize> {
    if process.stopped
        || root.tty == 0
        || process.tty != root.tty
        || process.group != process.foreground
    {
        return None;
    }
    let mut current = process;
    for depth in 0..128 {
        if current.pid == root.pid {
            return Some(depth);
        }
        current = processes.get(&current.parent)?;
    }
    None
}

fn live_ancestry(
    process: &Process,
    root: &Process,
    processes: &HashMap<i32, Process>,
) -> Result<bool> {
    let mut expected = process;
    for _ in 0..128 {
        let Some(current) = platform::process(expected.pid)? else {
            return Ok(false);
        };
        if current != *expected {
            return Ok(false);
        }
        if current.pid == root.pid {
            return Ok(true);
        }
        let Some(parent) = processes.get(&current.parent) else {
            return Ok(false);
        };
        expected = parent;
    }
    Ok(false)
}

/// Birth-bound authority for one interactive harness inside an authenticated
/// native server. This contains no native-owner or relay credentials.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeRoot {
    pub pid: i32,
    pub parent: i32,
    pub identity: String,
    pub harness: String,
    pub tty: u64,
    pub server: i32,
    pub server_identity: String,
    pub session_id: String,
}

fn native_lineage(root: &Process, server: &Process, processes: &HashMap<i32, Process>) -> bool {
    if root.stopped || root.tty == 0 || root.group != root.foreground {
        return false;
    }
    let mut current = root;
    for _ in 0..128 {
        if current.pid == server.pid {
            return true;
        }
        // A nested PTY is not a terminal owned by this native server. Every
        // process between the pane's initial child and the harness must retain
        // the same controlling terminal, including intervening shells.
        if current.tty != root.tty || current.stopped {
            return false;
        }
        let Some(parent) = processes.get(&current.parent) else {
            return false;
        };
        current = parent;
    }
    false
}

fn native_root(
    process: &Process,
    harness: &str,
    processes: &HashMap<i32, Process>,
    sessions: &[crate::context::LiveSession],
) -> Result<Option<NativeRoot>> {
    let mut selected = None;
    for session in sessions {
        let Some(server) = processes.get(&session.server) else {
            continue;
        };
        if !native_lineage(process, server, processes) {
            continue;
        }
        if selected.is_some() || !live_ancestry(process, server, processes)? {
            return Ok(None);
        }
        selected = Some(NativeRoot {
            pid: process.pid,
            parent: process.parent,
            identity: process.identity.clone(),
            harness: harness.to_owned(),
            tty: process.tty,
            server: server.pid,
            server_identity: server.identity.clone(),
            session_id: session.session_id.clone(),
        });
    }
    Ok(selected)
}

/// Resolve from OS ancestry, never from inherited public context or a claimed
/// harness session ID. Hook/MCP stdio and native tool PTYs are transport only;
/// they cannot replace the root's independently verified native terminal.
pub(crate) fn installed_ancestor(harness: &str) -> Result<Option<NativeRoot>> {
    let processes = platform::snapshot()?;
    let Some(helper) = processes.get(&(std::process::id() as i32)) else {
        return Ok(None);
    };
    let sessions = crate::context::live_sessions()?;
    let Some(process) =
        installed_candidate(helper, harness, &processes, &sessions, recognize_process)?
    else {
        return Ok(None);
    };
    let Some(root) = native_root(process, harness, &processes, &sessions)? else {
        return Ok(None);
    };
    if live_ancestry(helper, process, &processes)? {
        Ok(Some(root))
    } else {
        Ok(None)
    }
}

fn installed_candidate<'a>(
    helper: &'a Process,
    harness: &str,
    processes: &'a HashMap<i32, Process>,
    sessions: &[crate::context::LiveSession],
    mut recognize: impl FnMut(i32) -> Result<Option<&'static str>>,
) -> Result<Option<&'a Process>> {
    let mut current = helper;
    let mut selected = None;
    for _ in 0..128 {
        if let Some(recognized) = recognize(current.pid)?
            && !current.stopped
            && current.tty != 0
            && current.group == current.foreground
        {
            if !sessions.iter().any(|session| {
                processes
                    .get(&session.server)
                    .is_some_and(|server| native_lineage(current, server, processes))
            }) {
                // A recognized interactive harness in a nested/unmanaged PTY
                // cannot lend its callbacks to an outer native harness.
                return Ok(None);
            }
            if selected.is_some() {
                // A nested interactive harness must not borrow its parent's
                // managed identity, even when both executables have the same name.
                return Ok(None);
            }
            selected = Some((current, recognized));
        }
        let Some(parent) = processes.get(&current.parent) else {
            return Ok(selected
                .filter(|(_, recognized)| *recognized == harness)
                .map(|(process, _)| process));
        };
        current = parent;
    }
    // A cycle or an overlong ancestry is not partial authority.
    Ok(None)
}

pub(crate) fn revalidate_native_root(expected: &NativeRoot) -> Result<bool> {
    Ok(native_session(expected)?.is_some())
}

/// Revalidate only this root's ancestry, not the entire process table, and return
/// the authenticated owner publication used by startup's association barrier.
pub(crate) fn native_session(expected: &NativeRoot) -> Result<Option<crate::context::LiveSession>> {
    let mut processes = HashMap::new();
    let mut pid = expected.pid;
    for _ in 0..128 {
        let Some(process) = platform::process(pid)? else {
            return Ok(None);
        };
        let parent = process.parent;
        if processes.insert(pid, process).is_some() {
            return Ok(None);
        }
        if pid == expected.server {
            break;
        }
        pid = parent;
    }
    let Some(process) = processes.get(&expected.pid) else {
        return Ok(None);
    };
    if process.identity != expected.identity
        || process.parent != expected.parent
        || recognize_process(process.pid)? != Some(expected.harness.as_str())
    {
        return Ok(None);
    }
    for ancestor in processes.values() {
        if ancestor.pid != expected.pid
            && ancestor.pid != expected.server
            && ancestor.tty != 0
            && !ancestor.stopped
            && ancestor.group == ancestor.foreground
            && recognize_process(ancestor.pid)?.is_some()
        {
            return Ok(None);
        }
    }
    let Some(session) = crate::context::resolve_session(&expected.session_id)? else {
        return Ok(None);
    };
    if native_root(
        process,
        &expected.harness,
        &processes,
        std::slice::from_ref(&session),
    )?
    .as_ref()
        != Some(expected)
    {
        return Ok(None);
    }
    Ok(Some(session))
}

pub(crate) fn root_alive(root: &NativeRoot) -> Result<bool> {
    Ok(platform::process(root.pid)?
        .is_some_and(|process| process.identity == root.identity && process.parent == root.parent))
}

pub(crate) fn process_birth(pid: i32) -> Result<Option<String>> {
    Ok(platform::process(pid)?.map(|process| process.identity))
}

pub(crate) fn root_directory(root: &NativeRoot) -> Result<PathBuf> {
    platform::cwd(root.pid)?
        .map(PathBuf::from)
        .context("native harness root has no accessible working directory")
}

pub(crate) fn claude_resumed(root: &NativeRoot) -> Result<bool> {
    Ok(platform::arguments(root.pid)?
        .iter()
        .take_while(|argument| *argument != "--")
        .any(|argument| {
            argument.to_str().is_some_and(|argument| {
                matches!(argument, "--resume" | "-r" | "--continue" | "-c")
                    || argument.starts_with("--resume=")
            })
        }))
}

pub(crate) fn root_terminal(root: &NativeRoot) -> Result<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    #[cfg(target_os = "linux")]
    for descriptor in 0..3 {
        if let Ok(path) = std::fs::read_link(format!("/proc/{}/fd/{descriptor}", root.pid))
            && path.starts_with("/dev")
            && std::fs::metadata(&path).is_ok_and(|metadata| {
                metadata.file_type().is_char_device() && metadata.rdev() == root.tty
            })
        {
            return Ok(path);
        }
    }
    #[cfg(target_os = "macos")]
    for entry in std::fs::read_dir("/dev")? {
        let entry = entry?;
        if entry.file_name().as_encoded_bytes().starts_with(b"tty")
            && entry.metadata().is_ok_and(|metadata| {
                metadata.file_type().is_char_device() && metadata.rdev() == root.tty
            })
        {
            return Ok(entry.path());
        }
    }
    bail!("cannot resolve the verified native harness terminal")
}

#[cfg(any(test, target_os = "macos"))]
struct DarwinProcessArguments<'a> {
    argc: usize,
    arguments: &'a [u8],
    environment: &'a [u8],
}

#[cfg(any(test, target_os = "macos"))]
fn darwin_process_arguments(bytes: &[u8]) -> Option<DarwinProcessArguments<'_>> {
    if bytes.len() >= MAX_ARGUMENT_BYTES {
        return None;
    }
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    if !(1..=4096).contains(&argc) {
        return None;
    }
    // KERN_PROCARGS2 starts with argc, the executable path and NUL padding.
    let executable = bytes.get(4..)?;
    let executable_end = executable.iter().position(|byte| *byte == 0)?;
    if executable_end == 0 {
        return None;
    }
    let mut offset = 4 + executable_end + 1;
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    let arguments_start = offset;
    // Only the padding before argv[0] is skipped; empty later arguments count.
    for _ in 0..argc {
        let end = bytes.get(offset..)?.iter().position(|byte| *byte == 0)?;
        offset += end + 1;
    }
    let environment_start = offset;
    while offset < bytes.len() && bytes[offset] != 0 {
        let entry = bytes.get(offset..)?;
        let end = entry.iter().position(|byte| *byte == 0)?;
        if entry[..end].iter().position(|byte| *byte == b'=')? == 0 {
            return None;
        }
        offset += end + 1;
    }
    // Do not scan through the environment's NUL padding into Darwin's apple
    // strings. An unterminated environment entry is never usable authority.
    Some(DarwinProcessArguments {
        argc: argc as usize,
        arguments: &bytes[arguments_start..environment_start],
        environment: &bytes[environment_start..offset],
    })
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn relay_id_from_environment(bytes: &[u8], harness: &str) -> Option<String> {
    let mut variables = bytes.split(|byte| *byte == 0);
    let id = variables
        .clone()
        .find_map(|entry| entry.strip_prefix(b"BKPR_RELAY_AGENT_ID="))?;
    let declared = variables.find_map(|entry| entry.strip_prefix(b"BKPR_RELAY_HARNESS="))?;
    if declared != harness.as_bytes() || id.len() != 32 || !id.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(std::str::from_utf8(id).ok()?.to_owned())
}

#[cfg(target_os = "linux")]
fn relay_id(pid: i32, harness: &str) -> Option<String> {
    let process = File::open(format!("/proc/{pid}/environ")).ok()?;
    let bytes = bounded_read(process, 64 * 1024).ok()?;
    relay_id_from_environment(&bytes, harness)
}

#[cfg(target_os = "macos")]
fn relay_id(pid: i32, harness: &str) -> Option<String> {
    let bytes = platform::process_arguments(pid).ok()?;
    let arguments = darwin_process_arguments(&bytes)?;
    relay_id_from_environment(arguments.environment, harness)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn relay_id(_pid: i32, _harness: &str) -> Option<String> {
    None
}

pub(crate) fn root_launch(root: &NativeRoot) -> Option<crate::relay::RelayLaunch> {
    #[cfg(target_os = "linux")]
    let bytes = bounded_read(
        File::open(format!("/proc/{}/environ", root.pid)).ok()?,
        64 * 1024,
    )
    .ok()?;
    #[cfg(target_os = "macos")]
    let bytes = platform::process_arguments(root.pid).ok()?;
    #[cfg(target_os = "linux")]
    let environment = bytes.as_slice();
    #[cfg(target_os = "macos")]
    let environment = darwin_process_arguments(&bytes)?.environment;
    let launch = crate::relay::launch_from_environment(environment, &root.harness)?;
    crate::relay::verify_launch(&launch, root.pid as u32).ok()?;
    Some(launch)
}

pub fn inspect(json: &str) -> Result<InspectResponse> {
    if json.len() > MAX_MESSAGE_BYTES {
        bail!("inspect request exceeds message size limit");
    }
    let request: InspectRequest =
        serde_json::from_str(json).context("invalid agent inspect request")?;
    if request.panes.len() > MAX_PANES {
        bail!("inspect request exceeds {MAX_PANES} panes");
    }
    let processes = platform::snapshot()?;
    let mut sessions = Vec::new();
    let mut sessions_loaded = false;
    let mut foreground: HashMap<u64, Vec<&Process>> = HashMap::new();
    for process in processes.values() {
        if !process.stopped && process.tty != 0 && process.group == process.foreground {
            foreground.entry(process.tty).or_default().push(process);
        }
    }
    let mut executions = Vec::with_capacity(request.panes.len());
    for pane in request.panes {
        let Some(root) = processes.get(&pane.root_pid) else {
            continue;
        };
        let Some(candidates) = foreground.get(&root.tty) else {
            continue;
        };
        let mut selected: Option<(usize, i32, &Process, &str)> = None;
        for process in candidates {
            let Some(depth) = ancestry_depth(process, root, &processes) else {
                continue;
            };
            if let Some(harness) = recognize_process(process.pid)?
                && selected
                    .as_ref()
                    .is_none_or(|(best_depth, best_pid, _, _)| {
                        (depth, process.pid) < (*best_depth, *best_pid)
                    })
            {
                selected = Some((depth, process.pid, process, harness));
            }
        }
        let Some((_, _, process, harness)) = selected else {
            continue;
        };
        let Some(current_root) = platform::process(root.pid)? else {
            continue;
        };
        let Some(current) = platform::process(process.pid)? else {
            continue;
        };
        if current_root.identity != root.identity
            || current.identity != process.identity
            || current.stopped
            || current.tty != current_root.tty
            || current.group != current.foreground
        {
            continue;
        }
        // Both process reads enforce same-UID ownership. Keep metadata reads
        // inside the birth/ancestry checks so a reused PID cannot lend its ID.
        let cwd = platform::cwd(current.pid)?;
        let agent_id = if matches!(harness, "claude" | "omp" | "agy") {
            if !sessions_loaded {
                sessions = crate::context::live_sessions()?;
                sessions_loaded = true;
            }
            match native_root(process, harness, &processes, &sessions)? {
                Some(native) => crate::installed::lookup(&native)?
                    .or_else(|| root_launch(&native).map(|launch| launch.agent_id)),
                None => None,
            }
        } else {
            relay_id(current.pid, harness)
        };
        if !live_ancestry(process, root, &processes)? {
            continue;
        }
        executions.push(AgentExecution {
            pane_id: pane.pane_id,
            root_pid: pane.root_pid,
            pid: current.pid,
            identity: current.identity,
            harness: harness.to_owned(),
            cwd,
            agent_id,
        });
    }
    Ok(InspectResponse { executions })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn parse_stat(pid: i32, stat: &str, boot: &str) -> Option<Process> {
        let (_, tail) = stat.rsplit_once(") ")?;
        let mut fields = tail.split_whitespace();
        let state = fields.next()?;
        if matches!(state, "Z" | "X") {
            return None;
        }
        let parent = fields.next()?.parse().ok()?;
        let group = fields.next()?.parse().ok()?;
        let tty = fields.nth(1)?.parse::<i64>().ok()? as u64;
        let foreground = fields.next()?.parse().ok()?;
        let birth = fields.nth(13)?;
        Some(Process {
            pid,
            parent,
            group,
            tty,
            foreground,
            stopped: matches!(state, "T" | "t"),
            identity: format!("linux:{boot}:{birth}"),
        })
    }

    fn read_process(pid: i32, boot: &str) -> Result<Option<Process>> {
        if pid <= 0 {
            return Ok(None);
        }
        let directory = PathBuf::from(format!("/proc/{pid}"));
        if !fs::metadata(&directory)
            .is_ok_and(|metadata| metadata.uid() == unsafe { libc::geteuid() })
        {
            return Ok(None);
        }
        match fs::read_to_string(directory.join("stat")) {
            Ok(stat) => Ok(parse_stat(pid, &stat, boot)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn boot_identity() -> Result<&'static str> {
        static BOOT: std::sync::LazyLock<std::io::Result<String>> =
            std::sync::LazyLock::new(|| {
                fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .map(|boot| boot.trim().to_owned())
            });
        BOOT.as_ref()
            .map(String::as_str)
            .map_err(|error| anyhow::anyhow!("read Linux boot identity: {error}"))
    }

    pub fn process(pid: i32) -> Result<Option<Process>> {
        read_process(pid, boot_identity()?)
    }

    pub fn snapshot() -> Result<HashMap<i32, Process>> {
        let boot = boot_identity()?;
        let mut result = HashMap::new();
        let mut count = 0;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|pid| pid.parse::<i32>().ok())
            else {
                continue;
            };
            count += 1;
            if count > MAX_PROCESSES {
                bail!("process table exceeds inspection bound");
            }
            if let Some(process) = read_process(pid, boot)? {
                result.insert(pid, process);
            }
        }
        Ok(result)
    }

    pub fn executable(pid: i32) -> Result<PathBuf> {
        match fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(path) => Ok(path),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(PathBuf::new())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn arguments(pid: i32) -> Result<Vec<OsString>> {
        let file = match File::open(format!("/proc/{pid}/cmdline")) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        let bytes = bounded_read(file, MAX_ARGUMENT_BYTES)?;
        Ok(bytes
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(|value| OsString::from_vec(value.to_vec()))
            .collect())
    }

    pub fn cwd(pid: i32) -> Result<Option<String>> {
        match fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) => Ok(path.to_str().map(str::to_owned)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    // Public libproc ABI, proc_info.h: PROC_PIDTBSDINFO (3). Unlike ps lstart,
    // pbi_start_tvusec distinguishes PID reuse within the same second.
    #[repr(C)]
    struct BsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        parent: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        reserved: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        group: u32,
        jobc: u32,
        tty: u32,
        foreground: u32,
        nice: i32,
        start_sec: u64,
        start_usec: u64,
    }

    #[repr(C)]
    struct VnodePath {
        // vinfo_stat (136 bytes), vi_type, vi_pad, vi_fsid (8 bytes).
        vnode: [u64; 19],
        path: [u8; 1024],
    }

    #[repr(C)]
    struct VnodePaths {
        cwd: VnodePath,
        root: VnodePath,
    }

    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_listpids(kind: u32, value: u32, buffer: *mut libc::c_void, bytes: i32) -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            bytes: i32,
        ) -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut libc::c_void, bytes: u32) -> i32;
    }

    fn check_process_error() -> Result<()> {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ESRCH | libc::ENOENT | libc::EPERM | libc::EACCES)
        ) {
            Ok(())
        } else {
            Err(error).context("inspect Darwin process")
        }
    }

    pub fn process(pid: i32) -> Result<Option<Process>> {
        if pid <= 0 {
            return Ok(None);
        }
        let mut info: BsdInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<BsdInfo>();
        let count =
            unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut BsdInfo).cast(), size as i32) };
        if count <= 0 {
            check_process_error()?;
            return Ok(None);
        }
        if count as usize != size {
            bail!("libproc returned an incomplete process identity");
        }
        if info.uid != unsafe { libc::geteuid() } || info.status == 5 || info.flags & 4 != 0 {
            return Ok(None);
        }
        Ok(Some(Process {
            pid,
            parent: info.parent as i32,
            group: info.group as i32,
            foreground: info.foreground as i32,
            tty: if info.tty == u32::MAX {
                0
            } else {
                u64::from(info.tty)
            },
            stopped: info.status == 4,
            identity: format!("darwin:{}:{}", info.start_sec, info.start_usec),
        }))
    }

    pub fn snapshot() -> Result<HashMap<i32, Process>> {
        let mut pids = vec![0_i32; MAX_PROCESSES];
        let capacity = pids.len() * std::mem::size_of::<i32>();
        let bytes = unsafe {
            proc_listpids(
                4,
                libc::geteuid(),
                pids.as_mut_ptr().cast(),
                capacity as i32,
            )
        };
        if bytes <= 0 {
            return Err(std::io::Error::last_os_error()).context("list current-user processes");
        }
        if bytes as usize >= capacity {
            bail!("process table exceeds inspection bound");
        }
        let mut result = HashMap::new();
        for pid in pids
            .into_iter()
            .take(bytes as usize / std::mem::size_of::<i32>())
            .filter(|pid| *pid > 0)
        {
            if let Some(process) = process(pid)? {
                result.insert(pid, process);
            }
        }
        Ok(result)
    }

    pub fn executable(pid: i32) -> Result<PathBuf> {
        let mut bytes = [0_u8; 4096];
        let count = unsafe { proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if count <= 0 {
            check_process_error()?;
            return Ok(PathBuf::new());
        }
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        Ok(PathBuf::from(OsString::from_vec(bytes[..end].to_vec())))
    }

    pub fn process_arguments(pid: i32) -> Result<Vec<u8>> {
        // A full buffer may be a successful but truncated KERN_PROCARGS2 read.
        let mut mib = [libc::CTL_KERN, 49, pid];
        let mut bytes = vec![0_u8; MAX_ARGUMENT_BYTES];
        let mut size = bytes.len();
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(libc::ESRCH | libc::EPERM | libc::EACCES)
            ) {
                return Ok(Vec::new());
            }
            return Err(error).context("read Darwin process arguments");
        }
        if size >= bytes.len() {
            bail!("Darwin process arguments exceed inspection bound");
        }
        bytes.truncate(size);
        Ok(bytes)
    }

    pub fn arguments(pid: i32) -> Result<Vec<OsString>> {
        let bytes = process_arguments(pid)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let arguments =
            darwin_process_arguments(&bytes).context("invalid Darwin process arguments")?;
        Ok(arguments
            .arguments
            .split(|byte| *byte == 0)
            .take(arguments.argc)
            .map(|argument| OsString::from_vec(argument.to_vec()))
            .collect())
    }

    pub fn cwd(pid: i32) -> Result<Option<String>> {
        let mut paths: VnodePaths = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<VnodePaths>();
        let count = unsafe {
            proc_pidinfo(
                pid,
                9,
                0,
                (&mut paths as *mut VnodePaths).cast(),
                size as i32,
            )
        };
        if count <= 0 {
            check_process_error()?;
            return Ok(None);
        }
        if count as usize != size {
            bail!("incomplete Darwin process working directory");
        }
        let end = paths
            .cwd
            .path
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(paths.cwd.path.len());
        Ok(std::str::from_utf8(&paths.cwd.path[..end])
            .ok()
            .filter(|path| !path.is_empty())
            .map(str::to_owned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn darwin_fixture(argc: i32, body: &[u8]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn darwin_layout_separates_padding_empty_arguments_and_environment() {
        let id = "0123456789abcdef0123456789abcdef";
        let argv = b"node\0--require\0\0/opt/omp/cli.js\0BKPR_RELAY_AGENT_ID=decoy\0";
        let mut bytes = darwin_fixture(5, b"/opt/homebrew/bin/node\0\0\0");
        bytes.extend_from_slice(argv);
        bytes.extend_from_slice(b"NON_UTF8=\xff\0TOKEN=a=b\0");
        bytes.extend_from_slice(
            format!("BKPR_RELAY_AGENT_ID={id}\0BKPR_RELAY_HARNESS=omp\0").as_bytes(),
        );
        bytes.extend_from_slice(b"\0executable_path=/opt/homebrew/bin/node\0");

        let parsed = darwin_process_arguments(&bytes).expect("valid Darwin layout");
        assert_eq!(parsed.argc, 5);
        assert_eq!(parsed.arguments, argv);
        assert_eq!(
            relay_id_from_environment(parsed.environment, "omp").as_deref(),
            Some(id)
        );
        assert_eq!(
            relay_id_from_environment(parsed.environment, "claude"),
            None
        );
        assert!(
            !parsed
                .environment
                .windows(15)
                .any(|part| part == b"executable_path")
        );
    }

    #[test]
    fn darwin_environment_does_not_adopt_unmanaged_or_mismatched_harnesses() {
        let id = "0123456789abcdef0123456789abcdef";
        let mismatch = format!("BKPR_RELAY_AGENT_ID={id}\0BKPR_RELAY_HARNESS=claude\0");
        let invalid = b"BKPR_RELAY_AGENT_ID=invalid\0BKPR_RELAY_HARNESS=omp\0";
        let after_environment = format!("\0BKPR_RELAY_AGENT_ID={id}\0BKPR_RELAY_HARNESS=omp\0");
        for environment in [
            b"".as_slice(),
            b"PATH=/usr/bin\0".as_slice(),
            mismatch.as_bytes(),
            invalid.as_slice(),
            after_environment.as_bytes(),
        ] {
            let mut bytes = darwin_fixture(
                3,
                b"/opt/homebrew/bin/omp\0\0omp\0BKPR_RELAY_AGENT_ID=decoy\0BKPR_RELAY_HARNESS=omp\0",
            );
            bytes.extend_from_slice(environment);
            let parsed = darwin_process_arguments(&bytes).expect("valid Darwin layout");
            assert_eq!(relay_id_from_environment(parsed.environment, "omp"), None);
        }
    }

    #[test]
    fn darwin_parser_rejects_malformed_counts_strings_and_bounds() {
        for length in 0..4 {
            assert!(darwin_process_arguments(&[0; 4][..length]).is_none());
        }
        let malformed: &[(i32, &[u8])] = &[
            (-1, b"/usr/bin/omp\0omp\0"),
            (0, b"/usr/bin/omp\0omp\0"),
            (4097, b"/usr/bin/omp\0omp\0"),
            (1, b""),
            (1, b"/usr/bin/omp"),
            (1, b"/usr/bin/omp\0\0"),
            (1, b"\0omp\0PATH=/usr/bin\0"),
            (2, b"/usr/bin/omp\0omp\0"),
            (2, b"/usr/bin/omp\0omp\0unterminated"),
            (1, b"/usr/bin/omp\0omp\0BROKEN\0"),
            (1, b"/usr/bin/omp\0omp\0=empty-key\0"),
            (1, b"/usr/bin/omp\0omp\0PATH=unterminated"),
        ];
        for &(argc, body) in malformed {
            assert!(
                darwin_process_arguments(&darwin_fixture(argc, body)).is_none(),
                "argc={argc}, body={body:?}"
            );
        }
        let mut oversized = darwin_fixture(1, b"/usr/bin/omp\0omp\0PATH=/usr/bin\0");
        oversized.resize(MAX_ARGUMENT_BYTES, 0);
        assert!(darwin_process_arguments(&oversized).is_none());
        oversized.push(0);
        assert!(darwin_process_arguments(&oversized).is_none());
    }

    #[test]
    fn truncated_darwin_environment_never_exposes_launch_identity() {
        let id = "0123456789abcdef0123456789abcdef";
        let mut bytes = darwin_fixture(1, b"/usr/bin/omp\0\0omp\0");
        bytes.extend_from_slice(
            format!("BKPR_RELAY_AGENT_ID={id}\0BKPR_RELAY_HARNESS=omp\0").as_bytes(),
        );
        for end in 0..bytes.len() {
            let identity = darwin_process_arguments(&bytes[..end])
                .and_then(|parsed| relay_id_from_environment(parsed.environment, "omp"));
            assert_eq!(identity, None, "truncated at byte {end}");
        }
        let parsed = darwin_process_arguments(&bytes).expect("complete Darwin layout");
        assert_eq!(
            relay_id_from_environment(parsed.environment, "omp").as_deref(),
            Some(id)
        );
    }

    #[test]
    fn only_matching_managed_harness_identity_is_exposed() {
        let id = "0123456789abcdef0123456789abcdef";
        let environment =
            format!("UNRELATED=secret\0BKPR_RELAY_AGENT_ID={id}\0BKPR_RELAY_HARNESS=omp\0");
        assert_eq!(
            relay_id_from_environment(environment.as_bytes(), "omp").as_deref(),
            Some(id)
        );
        assert_eq!(
            relay_id_from_environment(environment.as_bytes(), "claude"),
            None
        );
        let malformed = b"BKPR_RELAY_AGENT_ID=not-an-id\0BKPR_RELAY_HARNESS=omp\0";
        assert_eq!(relay_id_from_environment(malformed, "omp"), None);
    }

    #[test]
    fn foreground_inspection_requires_matching_tty_and_live_ancestry() {
        let agent = Process {
            pid: 42,
            parent: 1,
            group: 42,
            foreground: 42,
            tty: 1,
            stopped: false,
            identity: "birth-2".into(),
        };
        let root = Process {
            pid: 1,
            parent: 0,
            group: 1,
            foreground: 42,
            tty: 1,
            stopped: false,
            identity: "root".into(),
        };
        let mut processes = HashMap::from([(1, root.clone()), (42, agent.clone())]);
        assert_eq!(ancestry_depth(&agent, &root, &processes), Some(1));
        let mut background = agent.clone();
        background.foreground = 99;
        assert_eq!(ancestry_depth(&background, &root, &processes), None);
        let mut stopped = agent.clone();
        stopped.stopped = true;
        assert_eq!(ancestry_depth(&stopped, &root, &processes), None);
        let mut other_tty = agent.clone();
        other_tty.tty = 2;
        assert_eq!(ancestry_depth(&other_tty, &root, &processes), None);
        processes.remove(&1);
        assert_eq!(ancestry_depth(&agent, &root, &processes), None);
    }

    #[test]
    fn recognition_uses_executable_or_known_node_entry_point() {
        assert_eq!(
            recognize(
                Path::new("/usr/bin/bash"),
                &["bash".into(), "-c".into(), "echo claude".into()]
            ),
            None
        );
        assert_eq!(
            recognize(
                Path::new("/usr/bin/node"),
                &[
                    "node".into(),
                    "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js".into()
                ]
            ),
            Some("claude")
        );
        for name in ["agy", "antigravity", ".agy-wrapped", ".antigravity-wrapped"] {
            assert_eq!(
                recognize(&Path::new("/usr/bin").join(name), &[]),
                Some("agy")
            );
        }
        assert_eq!(
            recognize(
                Path::new("/usr/bin/bash"),
                &["bash".into(), "-c".into(), "echo agy".into()]
            ),
            None
        );
    }

    fn recognize_arguments(executable: &str, arguments: &[&str]) -> Option<&'static str> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        recognize(Path::new(executable), &arguments)
    }

    #[test]
    fn packaged_runtime_entrypoints_survive_wrappers_and_runtime_options() {
        assert_eq!(
            recognize_arguments("/bin/.copilot-wrapped", &[]),
            Some("copilot")
        );
        assert_eq!(
            recognize_arguments("/npm/cline/bin/.cline", &[]),
            Some("cline")
        );
        for script in [
            "/nix/store/hash-cursor-agent/share/cursor-agent/index.js",
            "/home/user/.local/share/cursor-agent/versions/2026.10.01/index.js",
        ] {
            assert_eq!(
                recognize_arguments("/bin/node", &["agent", "--use-system-ca", script]),
                Some("cursor")
            );
        }
        assert_eq!(
            recognize_arguments(
                "/bin/node",
                &[
                    "node",
                    "--no-warnings",
                    "/npm/@github/copilot/npm-loader.js"
                ]
            ),
            Some("copilot")
        );
        assert_eq!(
            recognize_arguments("/bin/node", &["node", "/npm/cline/bin/cline"]),
            Some("cline")
        );
        assert_eq!(
            recognize_arguments("/bin/python3.14", &["python", "-u", "/venv/bin/aider"]),
            Some("aider")
        );
        assert_eq!(
            recognize_arguments("/bin/python3", &["python", "-X", "utf8", "-m", "aider"]),
            Some("aider")
        );
    }

    #[test]
    fn prompt_arguments_and_ambiguous_process_names_do_not_identify_harnesses() {
        let claude = "/npm/@anthropic-ai/claude-code/cli.js";
        for arguments in [
            vec!["node", "--eval", "void 0", claude],
            vec!["node", "--eval=void 0", claude],
            vec!["node", "--title", claude, "/tmp/ordinary.js"],
            vec!["node", "/tmp/ordinary.js", claude],
            vec!["node", "/tmp/index.js", "cursor-agent"],
            vec!["node", "/tmp/cursor-agent/index.js"],
            vec!["node", "--require", claude],
        ] {
            assert_eq!(recognize_arguments("/bin/node", &arguments), None);
        }
        assert_eq!(
            recognize_arguments("/bin/agent", &["cursor-agent", "copilot", "aider"]),
            None
        );
        assert_eq!(
            recognize_arguments("/bin/.copilot-wrapped-other", &[]),
            None
        );
        for arguments in [
            vec!["python", "-c", "import aider"],
            vec!["python", "-m", "ordinary_module", "aider"],
            vec!["python", "/tmp/ordinary.py", "/venv/bin/aider"],
        ] {
            assert_eq!(recognize_arguments("/bin/python3.14", &arguments), None);
        }
    }

    fn native_fixture() -> (HashMap<i32, Process>, Vec<crate::context::LiveSession>) {
        let process = |pid, parent, tty, group, foreground| Process {
            pid,
            parent,
            tty,
            group,
            foreground,
            stopped: false,
            identity: format!("fixture:birth:{pid}"),
        };
        let processes = HashMap::from([
            (10, process(10, 1, 0, 10, 0)),
            (11, process(11, 10, 17, 11, 20)),
            (20, process(20, 11, 17, 20, 20)),
            (21, process(21, 20, 99, 21, 21)),
            (22, process(22, 21, 99, 21, 21)),
        ]);
        let sessions = vec![crate::context::LiveSession {
            session_id: "a".repeat(48),
            session_name: "owned".into(),
            runtime_name: "owned-key".into(),
            server: 10,
            workspaces: Vec::new(),
            panes: Vec::new(),
        }];
        (processes, sessions)
    }

    #[test]
    fn installed_root_proves_native_terminal_not_native_tool_transport() {
        let (processes, sessions) = native_fixture();
        let selected = installed_candidate(&processes[&22], "agy", &processes, &sessions, |pid| {
            Ok((pid == 20).then_some("agy"))
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected.pid, 20);
        assert_ne!(processes[&22].tty, selected.tty);
        assert!(native_lineage(selected, &processes[&10], &processes));
        let mut nested = processes.clone();
        nested.get_mut(&11).unwrap().tty = 99;
        assert!(!native_lineage(&nested[&20], &nested[&10], &nested));
        assert!(
            installed_candidate(&nested[&22], "agy", &nested, &sessions, |pid| Ok((pid
                == 20)
                .then_some("agy")),)
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn installed_root_rejects_outside_wrong_harness_and_missing_uid_ancestry() {
        let (mut processes, sessions) = native_fixture();
        assert!(
            installed_candidate(&processes[&22], "omp", &processes, &sessions, |pid| Ok(
                (pid == 20).then_some("agy")
            ),)
            .unwrap()
            .is_none()
        );
        assert!(
            installed_candidate(&processes[&22], "agy", &processes, &[], |pid| Ok((pid
                == 20)
                .then_some("agy")),)
            .unwrap()
            .is_none()
        );
        // Same-UID platform snapshots omit a foreign owner, an exited parent,
        // and a reparented root equally: none supplies a complete native chain.
        processes.remove(&11);
        assert!(
            installed_candidate(&processes[&22], "agy", &processes, &sessions, |pid| Ok(
                (pid == 20).then_some("agy")
            ),)
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn nested_interactive_harness_cannot_adopt_parent_but_ttyless_ls_is_ignored() {
        let (mut processes, sessions) = native_fixture();
        for inner_harness in ["agy", "claude"] {
            assert!(
                installed_candidate(&processes[&22], "agy", &processes, &sessions, |pid| Ok(
                    match pid {
                        20 => Some("agy"),
                        21 => Some(inner_harness),
                        _ => None,
                    }
                ),)
                .unwrap()
                .is_none()
            );
        }
        processes.get_mut(&21).unwrap().tty = 17;
        assert!(
            installed_candidate(&processes[&22], "agy", &processes, &sessions, |pid| Ok(
                matches!(pid, 20 | 21).then_some("agy")
            ),)
            .unwrap()
            .is_none()
        );
        processes.get_mut(&21).unwrap().tty = 0;
        assert_eq!(
            installed_candidate(&processes[&22], "agy", &processes, &sessions, |pid| Ok(
                matches!(pid, 20 | 21).then_some("agy")
            ),)
            .unwrap()
            .unwrap()
            .pid,
            20
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_birth_liveness_rejects_reused_and_reparented_pid() {
        let process = platform::process(std::process::id() as i32)
            .unwrap()
            .unwrap();
        let root = NativeRoot {
            pid: process.pid,
            parent: process.parent,
            identity: process.identity,
            harness: "omp".into(),
            tty: process.tty,
            server: 1,
            server_identity: "unused".into(),
            session_id: "a".repeat(48),
        };
        assert!(root_alive(&root).unwrap());
        assert!(
            !root_alive(&NativeRoot {
                identity: "other-birth".into(),
                ..root.clone()
            })
            .unwrap()
        );
        assert!(!root_alive(&NativeRoot { parent: 0, ..root }).unwrap());
    }
}
