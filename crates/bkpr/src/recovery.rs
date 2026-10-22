//! Durable session-recovery records under `$XDG_STATE_HOME/bkpr/sessions/<hash>/recovery.json`.
//!
//! Only the launcher claims a record (`claim`); the native-owner plugin updates it through
//! `bkpr session-record`. An update is accepted only from the claimed `session_id` with a
//! non-decreasing sequence, so stale or foreign plugins can never overwrite a newer record.
//! Uses `flock` and Unix file modes, like the other state stores, so it is not Linux-gated.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bkpr::MAX_MESSAGE_BYTES;
use clap::ValueEnum;

use crate::harness::Harness;
use crate::relay::RecoveryAgents;
use bkpr::recovery_contract::{RECOVERY_VERSION, RecoveryPane, RecoveryRecord, RecoveryUpdate};

const FILE_NAME: &str = "recovery.json";
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// No record is claimed for this session (it predates recovery or was never launched
/// through the launcher). `bkpr session-record` maps it to exit code 3 so a reloaded
/// plugin stays silent.
#[derive(Debug)]
pub(crate) struct Unclaimed;

impl std::fmt::Display for Unclaimed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session recovery is not claimed for this session")
    }
}

impl std::error::Error for Unclaimed {}

fn state_root() -> Result<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
        .context("neither XDG_STATE_HOME nor HOME is an absolute path")
}

fn session_directory(root: &Path, session_name: &str) -> PathBuf {
    let hash = session_name
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    root.join("bkpr/sessions").join(format!("{hash:016x}"))
}

// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn record_path(session_name: &str) -> Result<PathBuf> {
    Ok(session_directory(&state_root()?, session_name).join(FILE_NAME))
}

// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn load(session_name: &str) -> Result<Option<RecoveryRecord>> {
    read_record(&session_directory(&state_root()?, session_name).join(FILE_NAME))
}

/// Launcher-only: make `session_id` the sole accepted writer, keeping `workspaces`
/// (restore) or clearing them (fresh/overwrite).
// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn claim(
    session_name: &str,
    session_id: &str,
    keep: Option<&RecoveryRecord>,
) -> Result<()> {
    claim_in(&state_root()?, session_name, session_id, keep)
}

pub(crate) fn record(json: &str) -> Result<()> {
    record_in(&state_root()?, json)
}

fn claim_in(
    root: &Path,
    session_name: &str,
    session_id: &str,
    keep: Option<&RecoveryRecord>,
) -> Result<()> {
    let directory = session_directory(root, session_name);
    fs::create_dir_all(
        directory
            .parent()
            .context("session directory has no parent")?,
    )?;
    crate::launcher::private_directory(&directory)?;
    let _lock = lock(&directory)?;
    write_record(
        &directory,
        &RecoveryRecord {
            version: RECOVERY_VERSION,
            session_name: session_name.to_owned(),
            session_id: session_id.to_owned(),
            sequence: 0,
            written_by: env!("CARGO_PKG_VERSION").to_owned(),
            updated_at_millis: now_millis(),
            workspaces: keep
                .map(|record| record.workspaces.clone())
                .unwrap_or_default(),
        },
    )
}

fn record_in(root: &Path, json: &str) -> Result<()> {
    if json.len() > MAX_MESSAGE_BYTES {
        bail!("recovery update exceeds {MAX_MESSAGE_BYTES} bytes");
    }
    let update: RecoveryUpdate = serde_json::from_str(json).context("parse recovery update")?;
    let directory = session_directory(root, &update.session_name);
    // Never create state for an unclaimed session.
    if !directory.is_dir() {
        return Err(Unclaimed.into());
    }
    let _lock = lock(&directory)?;
    let Some(current) = read_record(&directory.join(FILE_NAME))? else {
        return Err(Unclaimed.into());
    };
    if current.session_id != update.session_id {
        bail!(
            "recovery record is owned by session {}, not {}",
            current.session_id,
            update.session_id
        );
    }
    if update.sequence < current.sequence {
        bail!(
            "stale recovery update {} is older than {}",
            update.sequence,
            current.sequence
        );
    }
    if update.sequence == current.sequence {
        return Ok(());
    }
    write_record(
        &directory,
        &RecoveryRecord {
            version: RECOVERY_VERSION,
            session_name: update.session_name,
            session_id: update.session_id,
            sequence: update.sequence,
            written_by: env!("CARGO_PKG_VERSION").to_owned(),
            updated_at_millis: now_millis(),
            workspaces: update.workspaces,
        },
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn lock(directory: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("lock"))?;
    // SAFETY: the descriptor is owned by `file` for the duration of the call.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } == -1 {
        return Err(std::io::Error::last_os_error()).context("lock recovery record");
    }
    Ok(file)
}

fn read_record(path: &Path) -> Result<Option<RecoveryRecord>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("{} is not a current-user-owned file", path.display());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("{} exceeds {MAX_BYTES} bytes", path.display());
    }
    Ok(Some(
        RecoveryRecord::decode(&bytes).with_context(|| path.display().to_string())?,
    ))
}

/// Tempfile, fsync, rename, then fsync the directory. Caller holds the lock.
fn write_record(directory: &Path, record: &RecoveryRecord) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(temporary.as_file_mut(), record)?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(directory.join(FILE_NAME))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// How to restart one recorded pane.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaneLaunch {
    Shell {
        cwd: PathBuf,
        note: Option<String>,
    },
    Harness {
        harness: Harness,
        session_id: String,
        cwd: PathBuf,
        /// A Codex agent's own detached app-server manifest, to stop before resuming.
        codex_manifest: Option<PathBuf>,
    },
}

/// Decide how to restore a pane from relay state. Resumption never adopts a session another
/// live agent still holds; a Codex agent's own detached app-server is the sole exception.
// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn resolve(pane: &RecoveryPane, agents: &RecoveryAgents) -> PaneLaunch {
    let (cwd, cwd_note) = existing_cwd(pane.cwd.as_deref());
    let Some(agent_id) = pane.agent_id.as_deref() else {
        return PaneLaunch::Shell {
            cwd,
            note: cwd_note,
        };
    };
    let shell = |reason: &str| PaneLaunch::Shell {
        cwd: cwd.clone(),
        note: Some(match &cwd_note {
            Some(extra) => format!("{reason}; {extra}"),
            None => reason.to_owned(),
        }),
    };
    let agent = agents.by_id.get(agent_id);
    let Some((agent, session_id)) =
        agent.and_then(|agent| Some((agent, agent.session_id.as_deref()?)))
    else {
        return shell("no captured session to resume");
    };
    let Some(harness) = [Harness::Claude, Harness::Omp, Harness::Codex]
        .into_iter()
        .find(|harness| {
            harness
                .to_possible_value()
                .is_some_and(|v| v.matches(&agent.harness, false))
        })
    else {
        return shell("harness cannot be resumed");
    };
    let other_holder = agents
        .live_holders
        .get(session_id)
        .is_some_and(|holders| holders.iter().any(|holder| holder != agent_id));
    if other_holder {
        return shell("session is still open elsewhere");
    }
    let mut manifest = None;
    if agent.live {
        if harness != Harness::Codex {
            // The agent's own process still has its session open.
            return shell("session is still open elsewhere");
        }
        // Its detached app-server still owns the thread; resume only after stopping it.
        manifest = agent.queue_endpoint.as_deref().and_then(codex_manifest);
        if manifest.is_none() {
            return shell("codex server cannot be stopped safely");
        }
    }
    PaneLaunch::Harness {
        harness,
        session_id: session_id.to_owned(),
        cwd,
        codex_manifest: manifest,
    }
}

/// `unix:///tmp/bkpr-codex-*/server.sock` -> its `server.json`. Anything else is not a
/// location this process may later ask `codex::stop` to act on.
fn codex_manifest(endpoint: &str) -> Option<PathBuf> {
    let socket = Path::new(endpoint.strip_prefix("unix://")?);
    let runtime = socket.parent()?;
    (socket.file_name()? == "server.sock"
        && runtime.parent()? == Path::new("/tmp")
        && runtime
            .file_name()?
            .as_encoded_bytes()
            .starts_with(b"bkpr-codex-"))
    .then(|| runtime.join("server.json"))
}

/// The recorded directory, else its nearest existing ancestor, else `$HOME` (or `/`).
// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn existing_cwd(recorded: Option<&str>) -> (PathBuf, Option<String>) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_dir());
    existing_cwd_with(recorded, home.as_deref().unwrap_or(Path::new("/")))
}

fn existing_cwd_with(recorded: Option<&str>, fallback: &Path) -> (PathBuf, Option<String>) {
    let Some(recorded) = recorded.filter(|path| !path.is_empty()) else {
        return (fallback.to_owned(), None);
    };
    let path = Path::new(recorded);
    if path.is_absolute() {
        if path.is_dir() {
            return (path.to_owned(), None);
        }
        if let Some(ancestor) = path.ancestors().skip(1).find(|dir| dir.is_dir()) {
            return (
                ancestor.to_owned(),
                Some(format!("{recorded} no longer exists")),
            );
        }
    }
    (
        fallback.to_owned(),
        Some(format!("{recorded} no longer exists")),
    )
}

/// Arguments of the hidden `bkpr restore-pane` leaf helper, built by `restore_pane_args`.
#[derive(Clone, Debug, PartialEq, Eq, clap::Args)]
pub(crate) struct RestorePaneArgs {
    /// Restore-layout leaf key (`w{i}t{j}p{k}`).
    #[arg(long)]
    pub(crate) key: String,
    #[arg(long)]
    pub(crate) cwd: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) name: String,
    /// The recorded name was user-assigned and is restored through the owner plugin.
    #[arg(long)]
    pub(crate) sticky: bool,
    #[arg(long, value_enum)]
    pub(crate) harness: Option<Harness>,
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) session_id: Option<String>,
    #[arg(long)]
    pub(crate) codex_manifest: Option<PathBuf>,
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) note: Option<String>,
}

/// Owner-plugin CLI pipe that applies a restored pane's sticky (user-assigned) name.
const RESTORE_NAME_PIPE: &str = "bkpr-restore-name";

/// Restore one pane: enter its directory, re-apply its name, resume its harness session in
/// the foreground, then become an interactive shell there. Every failure is a note, never
/// an exit: the pane must always end as a usable shell.
pub(crate) fn restore_pane(args: RestorePaneArgs) -> Result<()> {
    let (cwd, cwd_note) = existing_cwd(args.cwd.as_deref());
    let mut notes: Vec<String> = args.note.into_iter().chain(cwd_note).collect();
    if let Err(error) = std::env::set_current_dir(&cwd) {
        notes.push(format!("cannot enter {}: {error}", cwd.display()));
    }
    let mut stdout = std::io::stdout();
    if args.sticky {
        if let Err(error) = send_restore_name(&args.key) {
            notes.push(format!("pane name not restored: {error:#}"));
        }
    } else {
        let _ = stdout
            .write_all(soft_title(&args.name).as_bytes())
            .and_then(|()| stdout.flush());
    }
    print_note(&notes);

    if let (Some(harness), Some(session_id)) = (args.harness, args.session_id.as_deref()) {
        let stopped = match &args.codex_manifest {
            // Only set for a Codex agent whose own detached app-server still holds the thread.
            Some(manifest) => match crate::codex::stop(manifest) {
                Ok(()) => true,
                Err(error) => {
                    print_note(&[format!(
                        "Codex session still owned by its previous server: {error:#}"
                    )]);
                    false
                }
            },
            None => true,
        };
        if stopped && let Err(error) = run_harness(harness, session_id, &cwd) {
            print_note(&[format!("cannot resume session: {error:#}")]);
        }
    }
    exec_shell(&cwd)
}

/// One dim stderr line; nothing for no notes.
fn print_note(notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    let text: String = notes
        .join("; ")
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    eprintln!("\x1b[2mbkpr: {text}\x1b[0m");
}

fn send_restore_name(key: &str) -> Result<()> {
    let pane_id = std::env::var("BKPR_PANE_ID").context("BKPR_PANE_ID is not set")?;
    let session = std::env::var("ZELLIJ_SESSION_NAME").context("ZELLIJ_SESSION_NAME is not set")?;
    let payload = serde_json::json!({ "key": key, "pane_id": pane_id }).to_string();
    crate::project_watch::send_pipe(&session, RESTORE_NAME_PIPE, &payload)
}

/// Run `bkpr run <harness> <resume>` as a foreground child sharing this terminal. Its exit
/// status is irrelevant: the shell follows either way.
fn run_harness(harness: Harness, session_id: &str, cwd: &Path) -> Result<()> {
    let name = harness
        .to_possible_value()
        .context("harness has no command-line name")?;
    let mut child = Command::new(std::env::current_exe().context("locate bkpr")?)
        .arg("run")
        .arg(name.get_name())
        .args(resume_arguments(harness, session_id))
        .current_dir(cwd)
        .env("PWD", cwd)
        .spawn()
        .context("start harness")?;
    // Terminal interrupts reach the whole foreground process group; they are the
    // harness's to handle, and must not kill the shell that follows it.
    ignore_terminal_signals(true);
    let waited = child.wait();
    ignore_terminal_signals(false);
    waited.context("wait for harness")?;
    Ok(())
}

/// Ignore (or restore the default for) SIGINT and SIGQUIT. Ignored dispositions survive
/// `exec`, so they are restored before the shell starts.
fn ignore_terminal_signals(ignore: bool) {
    let handler = if ignore { libc::SIG_IGN } else { libc::SIG_DFL };
    // SAFETY: setting a predefined disposition installs no Rust handler.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGQUIT, handler);
    }
}

/// Replace this process with the user's shell, interactive on the pane's terminal. It is
/// started like a normal pane's shell (no login flag), so the pane closes when it exits.
fn exec_shell(cwd: &Path) -> Result<()> {
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let error = Command::new(&shell).env("PWD", cwd).exec();
    print_note(&[format!(
        "cannot start {}: {error}",
        Path::new(&shell).display()
    )]);
    Err(Command::new("/bin/sh").env("PWD", cwd).exec()).context("start /bin/sh")
}

/// `bkpr restore-pane` arguments for one restore-layout leaf. `--opt=value` keeps values
/// that begin with `-` unambiguous.
// Consumed by the launcher once restore lands.
#[allow(dead_code)]
pub(crate) fn restore_pane_args(
    key: &str,
    pane: &RecoveryPane,
    launch: &PaneLaunch,
) -> Vec<String> {
    let mut args = vec![format!("--key={key}"), format!("--name={}", pane.name)];
    if pane.renamed {
        args.push("--sticky".to_owned());
    }
    match launch {
        PaneLaunch::Shell { cwd, note } => {
            args.push(format!("--cwd={}", cwd.display()));
            if let Some(note) = note {
                args.push(format!("--note={note}"));
            }
        }
        PaneLaunch::Harness {
            harness,
            session_id,
            cwd,
            codex_manifest,
        } => {
            args.push(format!("--cwd={}", cwd.display()));
            if let Some(name) = harness.to_possible_value() {
                args.push(format!("--harness={}", name.get_name()));
            }
            args.push(format!("--session-id={session_id}"));
            if let Some(manifest) = codex_manifest {
                args.push(format!("--codex-manifest={}", manifest.display()));
            }
        }
    }
    args
}

/// Harness arguments that resume `session_id`.
pub(crate) fn resume_arguments(harness: Harness, session_id: &str) -> Vec<OsString> {
    let flag = match harness {
        Harness::Codex => "resume",
        _ => "--resume",
    };
    vec![flag.into(), session_id.into()]
}

/// An OSC 2 window title. Zellij treats it as a soft pane title that the program may replace.
fn soft_title(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]2;{name}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::RecoveredAgent;
    use bkpr::recovery_contract::{RecoveryPane, RecoveryTab, RecoveryWorkspace, SplitNode};
    use std::os::unix::fs::PermissionsExt;

    fn workspaces() -> Vec<RecoveryWorkspace> {
        vec![RecoveryWorkspace {
            name: "w".into(),
            tabs: vec![RecoveryTab {
                name: "t".into(),
                focused: true,
                layout: SplitNode::Pane {
                    pane: RecoveryPane {
                        cwd: Some("/tmp".into()),
                        name: "p".into(),
                        renamed: false,
                        harness: None,
                        agent_id: None,
                        focused: true,
                    },
                },
            }],
        }]
    }

    fn agent(
        harness: &str,
        session: Option<&str>,
        endpoint: Option<&str>,
        live: bool,
    ) -> RecoveredAgent {
        RecoveredAgent {
            harness: harness.into(),
            session_id: session.map(Into::into),
            queue_endpoint: endpoint.map(Into::into),
            live,
        }
    }

    fn agents(entries: Vec<(&str, RecoveredAgent)>) -> RecoveryAgents {
        RecoveryAgents::from_entries(
            entries
                .into_iter()
                .map(|(id, agent)| (id.to_owned(), agent)),
        )
    }

    fn pane(agent_id: Option<&str>, cwd: &str) -> RecoveryPane {
        RecoveryPane {
            cwd: Some(cwd.into()),
            name: "p".into(),
            renamed: false,
            harness: None,
            agent_id: agent_id.map(Into::into),
            focused: false,
        }
    }

    fn note_of(launch: &PaneLaunch) -> Option<&str> {
        match launch {
            PaneLaunch::Shell { note, .. } => note.as_deref(),
            PaneLaunch::Harness { .. } => panic!("expected shell"),
        }
    }

    #[test]
    fn resume_arguments_per_harness() {
        assert_eq!(
            resume_arguments(Harness::Claude, "s1"),
            ["--resume", "s1"].map(OsString::from)
        );
        assert_eq!(
            resume_arguments(Harness::Omp, "s2"),
            ["--resume", "s2"].map(OsString::from)
        );
        assert_eq!(
            resume_arguments(Harness::Codex, "s3"),
            ["resume", "s3"].map(OsString::from)
        );
    }

    fn parse_restore(args: Vec<String>) -> RestorePaneArgs {
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(
            ["bkpr".to_owned(), "restore-pane".to_owned()]
                .into_iter()
                .chain(args),
        )
        .unwrap();
        let Some(crate::Commands::RestorePane(args)) = cli.command else {
            panic!("expected restore-pane");
        };
        args
    }

    #[test]
    fn restore_pane_args_roundtrip_through_clap() {
        let mut recorded = pane(Some("a"), "/srv/x");
        recorded.name = r#"-my "quoted" pane 'name'"#.into();
        recorded.renamed = true;
        let launch = PaneLaunch::Harness {
            harness: Harness::Codex,
            session_id: "-thread 1".into(),
            cwd: "/srv/x y".into(),
            codex_manifest: Some("/tmp/bkpr-codex-a/server.json".into()),
        };
        assert_eq!(
            parse_restore(restore_pane_args("w0t1p2", &recorded, &launch)),
            RestorePaneArgs {
                key: "w0t1p2".into(),
                cwd: Some("/srv/x y".into()),
                name: recorded.name.clone(),
                sticky: true,
                harness: Some(Harness::Codex),
                session_id: Some("-thread 1".into()),
                codex_manifest: Some("/tmp/bkpr-codex-a/server.json".into()),
                note: None,
            }
        );
        let mut plain = pane(None, "/tmp");
        plain.name = "two words".into();
        let launch = PaneLaunch::Shell {
            cwd: "/".into(),
            note: Some("-/tmp no longer exists; \"x\"".into()),
        };
        assert_eq!(
            parse_restore(restore_pane_args("w1t0p0", &plain, &launch)),
            RestorePaneArgs {
                key: "w1t0p0".into(),
                cwd: Some("/".into()),
                name: "two words".into(),
                sticky: false,
                harness: None,
                session_id: None,
                codex_manifest: None,
                note: Some("-/tmp no longer exists; \"x\"".into()),
            }
        );
    }

    #[test]
    fn soft_title_sequence() {
        assert_eq!(soft_title("a\x07b"), "\x1b]2;ab\x07");
        assert_eq!(soft_title("x\x1b]0;y\n"), "\x1b]2;x]0;y\x07");
    }

    #[test]
    fn no_agent_restores_shell() {
        let launch = resolve(&pane(None, "/tmp"), &RecoveryAgents::default());
        assert_eq!(
            launch,
            PaneLaunch::Shell {
                cwd: "/tmp".into(),
                note: None
            }
        );
    }

    #[test]
    fn unknown_agent_or_missing_session_id_notes() {
        let known = agents(vec![("a", agent("claude", None, None, false))]);
        for id in ["a", "ghost"] {
            let launch = resolve(&pane(Some(id), "/tmp"), &known);
            assert!(note_of(&launch).unwrap().contains("no captured session"));
        }
    }

    #[test]
    fn live_holder_refuses_resume() {
        let known = agents(vec![
            ("a", agent("claude", Some("s"), None, false)),
            ("b", agent("claude", Some("s"), None, true)),
        ]);
        let launch = resolve(&pane(Some("a"), "/tmp"), &known);
        assert!(note_of(&launch).unwrap().contains("still open elsewhere"));
    }

    #[test]
    fn codex_stale_server_is_the_only_exception() {
        let endpoint = "unix:///tmp/bkpr-codex-abc/server.sock";
        let alone = agents(vec![("a", agent("codex", Some("s"), Some(endpoint), true))]);
        match resolve(&pane(Some("a"), "/tmp"), &alone) {
            PaneLaunch::Harness {
                harness,
                session_id,
                codex_manifest,
                ..
            } => {
                assert_eq!(harness, Harness::Codex);
                assert_eq!(session_id, "s");
                assert_eq!(
                    codex_manifest,
                    Some("/tmp/bkpr-codex-abc/server.json".into())
                );
            }
            other => panic!("expected harness, got {other:?}"),
        }
        let shared = agents(vec![
            ("a", agent("codex", Some("s"), Some(endpoint), true)),
            ("b", agent("codex", Some("s"), None, true)),
        ]);
        let launch = resolve(&pane(Some("a"), "/tmp"), &shared);
        assert!(note_of(&launch).unwrap().contains("still open elsewhere"));
        // A foreign endpoint is never trusted as a manifest location.
        let foreign = agents(vec![(
            "a",
            agent("codex", Some("s"), Some("unix:///var/x/server.sock"), true),
        )]);
        let launch = resolve(&pane(Some("a"), "/tmp"), &foreign);
        assert!(
            note_of(&launch)
                .unwrap()
                .contains("cannot be stopped safely")
        );
        let no_endpoint = agents(vec![("a", agent("codex", Some("s"), None, true))]);
        let launch = resolve(&pane(Some("a"), "/tmp"), &no_endpoint);
        assert!(
            note_of(&launch)
                .unwrap()
                .contains("cannot be stopped safely")
        );
        // A dead Codex agent needs no manifest.
        let dead = agents(vec![("a", agent("codex", Some("s"), None, false))]);
        assert!(matches!(
            resolve(&pane(Some("a"), "/tmp"), &dead),
            PaneLaunch::Harness {
                codex_manifest: None,
                ..
            }
        ));
    }

    #[test]
    fn live_claude_or_omp_sole_holder_refuses_resume() {
        for name in ["claude", "omp"] {
            let known = agents(vec![("a", agent(name, Some("s"), None, true))]);
            let launch = resolve(&pane(Some("a"), "/tmp"), &known);
            assert!(note_of(&launch).unwrap().contains("still open elsewhere"));
        }
    }

    #[test]
    fn claude_and_omp_resolve_to_harness() {
        for (name, expected) in [("claude", Harness::Claude), ("omp", Harness::Omp)] {
            let known = agents(vec![("a", agent(name, Some("s"), None, false))]);
            assert_eq!(
                resolve(&pane(Some("a"), "/tmp"), &known),
                PaneLaunch::Harness {
                    harness: expected,
                    session_id: "s".into(),
                    cwd: "/tmp".into(),
                    codex_manifest: None,
                }
            );
        }
    }

    #[test]
    fn missing_cwd_falls_back_to_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let recorded = root.path().join("gone/deeper");
        let (cwd, note) = existing_cwd_with(recorded.to_str(), Path::new("/home/x"));
        assert_eq!(cwd, root.path());
        assert!(note.unwrap().contains("no longer exists"));
        let (cwd, note) = existing_cwd_with(Some("relative/gone"), Path::new("/"));
        assert_eq!(cwd, Path::new("/"));
        assert!(note.is_some());
        let (cwd, note) = existing_cwd_with(None, root.path());
        assert_eq!(cwd, root.path());
        assert!(note.is_none());
        let (cwd, note) = existing_cwd_with(root.path().to_str(), Path::new("/home/x"));
        assert_eq!(cwd, root.path());
        assert!(note.is_none());
    }

    fn update(session_id: &str, sequence: u64) -> String {
        serde_json::to_string(&RecoveryUpdate {
            session_name: "main".into(),
            session_id: session_id.into(),
            sequence,
            workspaces: workspaces(),
        })
        .unwrap()
    }

    fn file(root: &Path) -> PathBuf {
        session_directory(root, "main").join(FILE_NAME)
    }

    fn loaded(root: &Path) -> Option<RecoveryRecord> {
        read_record(&file(root)).unwrap()
    }

    #[test]
    fn claim_then_update_writes_atomically() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        record_in(root.path(), &update("s1", 1)).unwrap();
        let record = loaded(root.path()).unwrap();
        assert_eq!(record.sequence, 1);
        assert_eq!(record.workspaces, workspaces());
        assert_eq!(record.written_by, env!("CARGO_PKG_VERSION"));
        let mode = fs::metadata(file(root.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = fs::metadata(session_directory(root.path(), "main"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn foreign_owner_update_is_refused() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s2", None).unwrap();
        let before = fs::read(file(root.path())).unwrap();
        let error = record_in(root.path(), &update("s1", 1)).unwrap_err();
        assert!(error.downcast_ref::<Unclaimed>().is_none());
        assert_eq!(fs::read(file(root.path())).unwrap(), before);
    }

    #[test]
    fn stale_sequence_is_refused() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        record_in(root.path(), &update("s1", 5)).unwrap();
        assert!(record_in(root.path(), &update("s1", 4)).is_err());
        record_in(root.path(), &update("s1", 5)).unwrap();
        assert_eq!(loaded(root.path()).unwrap().sequence, 5);
    }

    #[test]
    fn unclaimed_update_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let error = record_in(root.path(), &update("s1", 1)).unwrap_err();
        assert!(error.downcast_ref::<Unclaimed>().is_some());
        // No state is created for an unclaimed session.
        assert!(!session_directory(root.path(), "main").exists());
    }

    #[test]
    fn claim_keep_preserves_workspaces() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        record_in(root.path(), &update("s1", 7)).unwrap();
        let old = loaded(root.path()).unwrap();
        claim_in(root.path(), "main", "s3", Some(&old)).unwrap();
        let record = loaded(root.path()).unwrap();
        assert_eq!(record.session_id, "s3");
        assert_eq!(record.sequence, 0);
        assert_eq!(record.workspaces, workspaces());
        claim_in(root.path(), "main", "s4", None).unwrap();
        assert!(loaded(root.path()).unwrap().workspaces.is_empty());
    }

    #[test]
    fn load_future_version_errors_without_touching_file() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        let future = br#"{"version":99,"anything":true}"#;
        fs::write(file(root.path()), future).unwrap();
        assert!(read_record(&file(root.path())).is_err());
        assert!(record_in(root.path(), &update("s1", 1)).is_err());
        assert_eq!(fs::read(file(root.path())).unwrap(), future);
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let json = " ".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(record_in(root.path(), &json).is_err());
    }
}
