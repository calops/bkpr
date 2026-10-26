//! Durable session-recovery records under `$XDG_STATE_HOME/bkpr/sessions/<hash>/recovery.json`.
//!
//! Only the launcher claims a record (`claim`); the native-owner plugin updates it through
//! `bkpr session-record`. An update is accepted only from the claimed `session_id` with a
//! non-decreasing sequence, so stale or foreign plugins can never overwrite a newer record.
//! Uses `flock` and Unix file modes, like the other state stores, so it is not Linux-gated.

use std::ffi::{OsStr, OsString};
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
use bkpr::recovery_contract::{
    RECOVERY_VERSION, RecoveryPane, RecoveryRecord, RecoveryUpdate, RestoreLink,
};

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

/// The claimed owner sent a sequence older than the record's (its model was recreated and
/// restarted counting). `bkpr session-record` exits 4 and prints `current` on stdout so the
/// owner continues past it at once.
#[derive(Debug)]
pub(crate) struct StaleSequence {
    pub(crate) current: u64,
}

impl std::fmt::Display for StaleSequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stale recovery update; the record is at sequence {}",
            self.current
        )
    }
}

impl std::error::Error for StaleSequence {}

/// Neither `XDG_STATE_HOME` nor `HOME` locates a state directory, so recovery is off.
/// Distinct from an unreadable record: launching fresh cannot discard anything.
#[derive(Debug)]
pub(crate) struct StateUnavailable;

impl std::fmt::Display for StateUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("neither XDG_STATE_HOME nor HOME is an absolute path")
    }
}

impl std::error::Error for StateUnavailable {}

fn state_root() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
        .ok_or(StateUnavailable)?)
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

pub(crate) fn load(session_name: &str) -> Result<Option<RecoveryRecord>> {
    load_in(&state_root()?, session_name)
}

fn load_in(root: &Path, session_name: &str) -> Result<Option<RecoveryRecord>> {
    read_named(
        &session_directory(root, session_name).join(FILE_NAME),
        session_name,
    )
}

/// `read_record`, refusing a record of another session whose name hashes to the same
/// directory, like an unreadable one.
fn read_named(path: &Path, session_name: &str) -> Result<Option<RecoveryRecord>> {
    let record = read_record(path)?;
    if let Some(record) = &record
        && record.session_name != session_name
    {
        bail!(
            "{} belongs to another session, not {session_name:?}",
            path.display()
        );
    }
    Ok(record)
}

/// Launcher-only: make `session_id` the sole accepted writer, keeping `workspaces`
/// (restore) or clearing them (fresh/overwrite).
pub(crate) fn claim(
    session_name: &str,
    session_id: &str,
    keep: Option<&RecoveryRecord>,
) -> Result<()> {
    claim_in(&state_root()?, session_name, session_id, keep)
}

pub(crate) fn record(json: &str) -> Result<()> {
    record_with(state_root(), json)
}

/// Without a state root recovery is silently off, like an unclaimed session.
fn record_with(root: Result<PathBuf>, json: &str) -> Result<()> {
    let root = root.map_err(|error| {
        if error.downcast_ref::<StateUnavailable>().is_some() {
            Unclaimed.into()
        } else {
            error
        }
    })?;
    record_in(&root, json)
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
    // An unreadable record may be discarded (`--overwrite`); another session's never is.
    if let Ok(Some(existing)) = read_record(&directory.join(FILE_NAME))
        && existing.session_name != session_name
    {
        bail!(
            "the recovery record at {} belongs to another session",
            directory.display()
        );
    }
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
    let Some(current) = read_named(&directory.join(FILE_NAME), &update.session_name)? else {
        return Err(Unclaimed.into());
    };
    if current.session_id != update.session_id {
        bail!(
            "recovery record is owned by session {}, not {}",
            current.session_id,
            update.session_id
        );
    }
    // An equal sequence is an idempotent retry only with identical contents; otherwise,
    // like an older one, the owner restarted counting and must continue past the record.
    if update.sequence < current.sequence
        || (update.sequence == current.sequence && update.workspaces != current.workspaces)
    {
        return Err(StaleSequence {
            current: current.sequence,
        }
        .into());
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
        /// The refusal is transient (session held elsewhere, unsafe Codex stop, unreadable
        /// relay state): the pane keeps its recorded association so a later resume retries.
        retry: bool,
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
pub(crate) fn resolve(pane: &RecoveryPane, agents: &RecoveryAgents) -> PaneLaunch {
    let (cwd, cwd_note) = existing_cwd(pane.cwd.as_deref());
    let Some(agent_id) = pane.agent_id.as_deref() else {
        return PaneLaunch::Shell {
            cwd,
            note: cwd_note,
            retry: false,
        };
    };
    let refuse = |reason: &str, retry: bool| PaneLaunch::Shell {
        cwd: cwd.clone(),
        note: Some(match &cwd_note {
            Some(extra) => format!("{reason}; {extra}"),
            None => reason.to_owned(),
        }),
        retry,
    };
    // No captured session or an unsupported harness cannot succeed later either.
    let shell = |reason: &str| refuse(reason, false);
    let retry_later = |reason: &str| refuse(reason, true);
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
        return retry_later("session is still open elsewhere");
    }
    let mut manifest = None;
    if agent.live {
        if harness != Harness::Codex {
            // The agent's own process still has its session open.
            return retry_later("session is still open elsewhere");
        }
        // Its detached app-server still owns the thread; resume only after stopping it.
        manifest = agent.queue_endpoint.as_deref().and_then(codex_manifest);
        if manifest.is_none() {
            return retry_later("codex server cannot be stopped safely");
        }
    }
    PaneLaunch::Harness {
        harness,
        session_id: session_id.to_owned(),
        cwd,
        codex_manifest: manifest,
    }
}

/// Restore `pane` as a shell because relay state is unreadable (`reason`). Harness panes
/// note why they were not resumed; resume itself never fails for this.
pub(crate) fn unresolved(pane: &RecoveryPane, reason: &str) -> PaneLaunch {
    let (cwd, cwd_note) = existing_cwd(pane.cwd.as_deref());
    let harness = pane.agent_id.is_some() || pane.harness.is_some();
    let note = match (harness, cwd_note) {
        (false, note) => note,
        (true, None) => Some(format!("relay state was unreadable: {reason}")),
        (true, Some(extra)) => Some(format!("relay state was unreadable: {reason}; {extra}")),
    };
    PaneLaunch::Shell {
        cwd,
        note,
        retry: harness,
    }
}

/// The recorded association `pane` keeps for a later retry if this restore does not
/// resume it: every resume attempt, and every transient refusal. A detected agent or the
/// pane closing ends it (owner-side).
pub(crate) fn restore_link(pane: &RecoveryPane, launch: &PaneLaunch) -> Option<RestoreLink> {
    let attempted = match launch {
        PaneLaunch::Harness { .. } => true,
        PaneLaunch::Shell { retry, .. } => *retry,
    };
    Some(RestoreLink {
        harness: pane.harness.clone()?,
        agent_id: pane.agent_id.clone()?,
    })
    .filter(|_| attempted)
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
    // A restore layout (KDL) cannot carry it, so it is never used even if it exists.
    if recorded.chars().any(char::is_control) {
        return (
            fallback.to_owned(),
            Some("recorded directory name has control characters".to_owned()),
        );
    }
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
    /// Recorded association the owner keeps for a retry until an agent appears here.
    #[arg(long, requires = "link_agent_id")]
    pub(crate) link_harness: Option<String>,
    #[arg(long, allow_hyphen_values = true, requires = "link_harness")]
    pub(crate) link_agent_id: Option<String>,
}

/// Owner-plugin CLI pipe that applies a restored pane's sticky (user-assigned) name.
const RESTORE_NAME_PIPE: &str = "bkpr-restore-name";

/// Restore one pane: enter its directory, re-apply its name, type its harness resume command
/// for the shell, then become that interactive shell. Every failure is a note, never
/// an exit: the pane must always end as a usable shell.
pub(crate) fn restore_pane(args: RestorePaneArgs) -> Result<()> {
    set_setup_signals(libc::SIG_IGN);
    let (cwd, cwd_note) = existing_cwd(args.cwd.as_deref());
    let mut notes: Vec<String> = args.note.into_iter().chain(cwd_note).collect();
    if let Err(error) = std::env::set_current_dir(&cwd) {
        notes.push(format!("cannot enter {}: {error}", cwd.display()));
    }
    let mut stdout = std::io::stdout();
    let link = args.link_harness.clone().zip(args.link_agent_id.clone());
    // One claim carries the sticky name and the retained link, before any resume attempt,
    // so a refusal, a failed stop or a harness that exits at once keeps the association.
    if (args.sticky || link.is_some())
        && let Err(error) = send_restore_claim(&args.key, link.as_ref())
        && let Some(note) = restore_claim_note(&error, args.sticky)
    {
        notes.push(note);
    }
    if !args.sticky {
        let _ = stdout
            .write_all(soft_title(&args.name).as_bytes())
            .and_then(|()| stdout.flush());
    }
    print_note(&notes);

    if let (Some(harness), Some(session_id)) = (args.harness, args.session_id.as_deref()) {
        let stopped = match &args.codex_manifest {
            // Only set for a Codex agent whose own detached app-server still holds the thread.
            // Stopping retires its broker route, so start the shared broker first when none
            // runs (it may have ended with the closed session). A live one is never replaced.
            Some(manifest) => match crate::relay::ensure_broker() {
                Err(error) => {
                    print_note(&[format!("relay broker unavailable: {error:#}")]);
                    false
                }
                Ok(_) => match crate::codex::stop(manifest) {
                    Ok(()) => true,
                    Err(error) => {
                        print_note(&[codex_stop_note(&error)]);
                        false
                    }
                },
            },
            None => true,
        };
        if stopped {
            queue_resume(harness, session_id, &cwd);
        }
    }
    exec_shell(&cwd)
}

/// Why a Codex agent's previous server could not be stopped: a broker that is not there,
/// pending messages the broker refuses to drop, or any other refusal.
fn codex_stop_note(error: &anyhow::Error) -> String {
    if error
        .downcast_ref::<crate::relay::BrokerUnavailable>()
        .is_some()
    {
        format!("relay broker unavailable; Codex session left with its previous server: {error:#}")
    } else if error
        .chain()
        .any(|cause| cause.to_string() == crate::relay::UNACKNOWLEDGED_STOP)
    {
        "Codex session has unacknowledged messages; its previous server keeps it until they are read and acknowledged".to_owned()
    } else {
        format!("Codex session still owned by its previous server: {error:#}")
    }
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

fn send_restore_claim(key: &str, link: Option<&(String, String)>) -> Result<()> {
    let pane_id = std::env::var("BKPR_PANE_ID").context("BKPR_PANE_ID is not set")?;
    let session = std::env::var("ZELLIJ_SESSION_NAME").context("ZELLIJ_SESSION_NAME is not set")?;
    let mut payload = serde_json::json!({ "key": key, "pane_id": pane_id });
    if let Some((harness, agent_id)) = link {
        payload["harness"] = harness.as_str().into();
        payload["agent_id"] = agent_id.as_str().into();
    }
    let payload = payload.to_string();
    crate::project_watch::send_pipe(&session, RESTORE_NAME_PIPE, &payload)
}

/// The command line that resumes a session, with the positions of the flags it generated:
/// `--resume` before the id (Claude, omp) or `-C` before the directory (Codex).
#[derive(Debug, PartialEq, Eq)]
struct ResumeCommand {
    words: Vec<OsString>,
    flags: Vec<usize>,
}

/// `<program> run <harness> <resume_arguments…>`.
fn resume_command(program: &Path, harness: Harness, session_id: &str, cwd: &Path) -> ResumeCommand {
    let mut words = vec![program.as_os_str().to_owned(), "run".into()];
    if let Some(name) = harness.to_possible_value() {
        words.push(name.get_name().into());
    }
    let start = words.len();
    let arguments = resume_arguments(harness, session_id, cwd);
    // Our flags sit at fixed positions of `resume_arguments`: `--resume <id>` and
    // `resume <id> -C <cwd>`. Values there never count as flags.
    let flags = match harness {
        Harness::Codex => vec![start + 2],
        _ => vec![start],
    };
    words.extend(arguments);
    ResumeCommand { words, flags }
}

/// A word that fish, bash and zsh all read back literally when typed unquoted, and that no
/// program can take for an option. A leading `%` (fish `%self`) or `=` (zsh `=cmd`) would
/// be expanded.
fn typeable_word(word: &OsStr) -> bool {
    let Some(word) = word.to_str() else {
        return false;
    };
    let neutral =
        |character: char| character.is_ascii_alphanumeric() || "_./:@%+=,-".contains(character);
    !word.is_empty() && word.chars().all(neutral) && !word.starts_with(['%', '=', '-'])
}

/// Words that may stay bare: generated flags at their own positions, else neutral words.
fn bare_words(command: &ResumeCommand) -> impl Iterator<Item = (&OsString, bool)> {
    command.words.iter().enumerate().map(|(index, word)| {
        let flag =
            command.flags.contains(&index) && RESUME_FLAGS.contains(&&*word.to_string_lossy());
        (word, flag || typeable_word(word))
    })
}

/// Flags `resume_arguments` generates.
const RESUME_FLAGS: [&str; 2] = ["--resume", "-C"];

/// The line to type, or `None` when any word would need quoting.
fn typeable_command(command: &ResumeCommand) -> Option<String> {
    bare_words(command)
        .map(|(word, bare)| bare.then(|| word.to_string_lossy()))
        .collect::<Option<Vec<_>>>()
        .map(|words| words.join(" "))
}

/// POSIX-quoted command line for a note: bare words stay bare.
fn quoted_command(command: &ResumeCommand) -> String {
    bare_words(command)
        .map(|(word, bare)| {
            if bare {
                word.to_string_lossy().into_owned()
            } else {
                crate::claude::shell_quote(&word.to_string_lossy())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Type the resume command into this pane through `write(pane_id, text)`, where the shell
/// exec'd next reads it as if the user had typed it, so the harness is an ordinary job
/// (suspend, `fg`, history). Returns a note with the command when nothing was typed.
fn offer_resume(
    command: &ResumeCommand,
    pane_id: Option<&str>,
    write: impl FnOnce(&str, &str) -> Result<()>,
) -> Option<String> {
    let typed = typeable_command(command)
        .zip(pane_id)
        .is_some_and(|(line, pane)| write(pane, &format!("{line}\n")).is_ok());
    (!typed).then(|| format!("resume this session with: {}", quoted_command(command)))
}

/// Queue `bkpr run <harness> <resume>` as typed input for the shell that replaces this
/// helper, or note the command when it cannot be typed safely.
fn queue_resume(harness: Harness, session_id: &str, cwd: &Path) {
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => {
            print_note(&[format!("cannot resume session: locate bkpr: {error}")]);
            return;
        }
    };
    let command = resume_command(&program, harness, session_id, cwd);
    let pane_id = std::env::var("BKPR_PANE_ID").ok();
    let note = offer_resume(&command, pane_id.as_deref(), |pane, text| {
        let session =
            std::env::var("ZELLIJ_SESSION_NAME").context("ZELLIJ_SESSION_NAME is not set")?;
        // The line discipline would echo the text before the shell's prompt, and the line
        // editor shows it again: hide it until it is queued. Dropped before the shell starts.
        let _echo = EchoGuard::new(libc::STDIN_FILENO);
        crate::project_watch::write_chars(&session, pane, text)?;
        wait_queued(libc::STDIN_FILENO, text.len(), QUEUE_WAIT);
        Ok(())
    });
    if let Some(note) = note {
        print_note(&[note]);
    }
}

/// How long the helper waits for typed text to reach its terminal before starting the shell.
const QUEUE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Clears ECHO on a terminal and restores its saved settings on drop, without flushing the
/// queued input (`TCSANOW`, never `TCSAFLUSH`).
struct EchoGuard {
    fd: libc::c_int,
    saved: libc::termios,
}

impl EchoGuard {
    /// `None` when `fd` is not a terminal or its settings cannot be changed.
    fn new(fd: libc::c_int) -> Option<Self> {
        // SAFETY: `termios` is plain data and `tcgetattr` fully initializes it on success.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointer; a non-terminal fd fails with ENOTTY.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: valid termios pointer.
        (unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) } == 0).then_some(Self { fd, saved })
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        // SAFETY: restores the settings read from the same fd.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// Wait until at least `bytes` bytes of input are queued on terminal `fd` (in canonical
/// mode, complete lines only), or `timeout` passes. Returns whether they arrived.
fn wait_queued(fd: libc::c_int, bytes: usize, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut queued: libc::c_int = 0;
        // SAFETY: valid out pointer.
        if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) } == 0
            && usize::try_from(queued).unwrap_or(0) >= bytes
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Terminal signals the helper ignores while it sets up, so an early Ctrl+C or Ctrl+Z cannot
/// kill or stop it before its shell starts. Reset to defaults right before the exec, since
/// ignored dispositions survive it.
const SETUP_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP];

fn set_setup_signals(handler: libc::sighandler_t) {
    for signal in SETUP_SIGNALS {
        // SAFETY: SIG_IGN/SIG_DFL install no Rust handler.
        unsafe {
            libc::signal(signal, handler);
        }
    }
}

/// Note for a failed sticky-name request. A timed-out pipe was still delivered to the
/// owner, which applies the queued claim once it is granted.
fn restore_claim_note(error: &anyhow::Error, sticky: bool) -> Option<String> {
    if error
        .downcast_ref::<crate::project_watch::TimedOut>()
        .is_some()
    {
        sticky.then(|| "pane name will apply when the session is ready".to_owned())
    } else if sticky {
        Some(format!("pane name not restored: {error:#}"))
    } else {
        Some(format!("resume link not kept for a retry: {error:#}"))
    }
}

/// Replace this process with the user's shell, interactive on the pane's terminal. It is
/// started like a normal pane's shell (no login flag), so the pane closes when it exits.
fn exec_shell(cwd: &Path) -> Result<()> {
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    // Never hand the shell the dispositions ignored during setup.
    set_setup_signals(libc::SIG_DFL);
    let error = Command::new(&shell).env("PWD", cwd).exec();
    print_note(&[format!(
        "cannot start {}: {error}",
        Path::new(&shell).display()
    )]);
    Err(Command::new("/bin/sh").env("PWD", cwd).exec()).context("start /bin/sh")
}

/// `bkpr restore-pane` arguments for one restore-layout leaf. `--opt=value` keeps values
/// that begin with `-` unambiguous.
pub(crate) fn restore_pane_args(
    key: &str,
    pane: &RecoveryPane,
    launch: &PaneLaunch,
) -> Vec<String> {
    let mut args = vec![format!("--key={key}"), format!("--name={}", pane.name)];
    if pane.renamed {
        args.push("--sticky".to_owned());
    }
    if let Some(link) = restore_link(pane, launch) {
        args.push(format!("--link-harness={}", link.harness));
        args.push(format!("--link-agent-id={}", link.agent_id));
    }
    match launch {
        PaneLaunch::Shell { cwd, note, .. } => {
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

/// Harness arguments that resume `session_id` in `cwd`. Codex is told the directory
/// explicitly (`-C`), so a fallback cwd does not trigger its directory-change prompt; Bkpr's
/// Codex launcher also starts its app-server there.
pub(crate) fn resume_arguments(harness: Harness, session_id: &str, cwd: &Path) -> Vec<OsString> {
    match harness {
        Harness::Codex => vec![
            "resume".into(),
            session_id.into(),
            "-C".into(),
            cwd.as_os_str().to_owned(),
        ],
        _ => vec!["--resume".into(), session_id.into()],
    }
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
    use std::os::unix::ffi::OsStringExt;
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
        let cwd = Path::new("/srv/x y");
        assert_eq!(
            resume_arguments(Harness::Claude, "s1", cwd),
            ["--resume", "s1"].map(OsString::from)
        );
        assert_eq!(
            resume_arguments(Harness::Omp, "s2", cwd),
            ["--resume", "s2"].map(OsString::from)
        );
        // Codex names the directory explicitly so a fallback cwd does not prompt.
        assert_eq!(
            resume_arguments(Harness::Codex, "s3", cwd),
            ["resume", "s3", "-C", "/srv/x y"].map(OsString::from)
        );
    }

    fn words(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    const BKPR: &str = "/bin/bkpr";

    fn command(harness: Harness, session_id: &str, cwd: &str) -> ResumeCommand {
        resume_command(Path::new(BKPR), harness, session_id, Path::new(cwd))
    }

    #[test]
    fn resume_command_per_harness() {
        assert_eq!(
            command(Harness::Claude, "s1", "/srv/p").words,
            words(&[BKPR, "run", "claude", "--resume", "s1"])
        );
        assert_eq!(
            command(Harness::Omp, "s2", "/srv/p").words,
            words(&[BKPR, "run", "omp", "--resume", "s2"])
        );
        assert_eq!(
            command(Harness::Codex, "s3", "/srv/p").words,
            words(&[BKPR, "run", "codex", "resume", "s3", "-C", "/srv/p"])
        );
    }

    #[test]
    fn typeable_words_are_shell_neutral() {
        for word in ["/a/b-c_d.e", "run", "agy", "0199-abc:x@y%z+w=v,u"] {
            assert!(typeable_word(OsStr::new(word)), "{word}");
        }
        for word in [
            "", "a b", "a'b", "a\"b", "$HOME", "a*", "a;b", "~/x", "a\nb", "(x)", "{a,b}", "#x",
            "a!", "-", "-x", "--yolo",
            // Only the generated flags, at their own positions, may start with `-`.
            "--resume", "-C",
            // Expanded by fish (`%self`) or zsh (`=cmd`) at the start of a word.
            "%self", "=ls", "é",
        ] {
            assert!(!typeable_word(OsStr::new(word)), "{word:?}");
        }
        assert!(!typeable_word(&OsString::from_vec(vec![b'a', 0xff])));
    }

    #[test]
    fn typeable_command_is_the_joined_line_only_when_every_word_is_safe() {
        assert_eq!(
            typeable_command(&command(Harness::Codex, "s", "/srv/p")).as_deref(),
            Some("/bin/bkpr run codex resume s -C /srv/p")
        );
        assert_eq!(
            typeable_command(&command(Harness::Claude, "s", "/srv/p")).as_deref(),
            Some("/bin/bkpr run claude --resume s")
        );
        assert_eq!(
            typeable_command(&command(Harness::Codex, "s", "/srv/x y")),
            None
        );
    }

    #[test]
    fn flags_are_accepted_only_at_their_generated_positions() {
        // A session id equal to (or shaped like) a flag is never typed.
        for id in ["--resume", "-C", "--rm", "-"] {
            for harness in [Harness::Claude, Harness::Omp, Harness::Codex] {
                let resume = command(harness, id, "/srv/p");
                assert_eq!(typeable_command(&resume), None, "{harness:?} {id}");
                let note = offer_resume(&resume, Some("1"), |_, _| panic!("typed {id}"));
                assert!(note.unwrap().starts_with("resume this session with: "));
            }
        }
    }

    #[test]
    fn fallback_note_quotes_a_cwd_with_a_space() {
        let mut typed = Vec::new();
        let note = offer_resume(
            &command(Harness::Codex, "s", "/srv/x y's"),
            Some("7"),
            |pane, text| {
                typed.push((pane.to_owned(), text.to_owned()));
                Ok(())
            },
        );
        assert!(typed.is_empty());
        assert_eq!(
            note.as_deref(),
            Some("resume this session with: /bin/bkpr run codex resume s -C '/srv/x y'\\''s'")
        );
        // A flag-shaped session id is quoted in the note, generated flags stay bare.
        assert_eq!(
            offer_resume(&command(Harness::Claude, "--rm", "/"), None, |_, _| Ok(())).as_deref(),
            Some("resume this session with: /bin/bkpr run claude --resume '--rm'")
        );
    }

    #[test]
    fn missing_pane_id_types_nothing_and_notes() {
        let mut called = false;
        let note = offer_resume(&command(Harness::Claude, "s1", "/"), None, |_, _| {
            called = true;
            Ok(())
        });
        assert!(!called);
        assert_eq!(
            note.as_deref(),
            Some("resume this session with: /bin/bkpr run claude --resume s1")
        );
    }

    #[test]
    fn failed_write_notes_the_command() {
        let note = offer_resume(&command(Harness::Claude, "s1", "/"), Some("3"), |_, _| {
            bail!("no server")
        });
        assert_eq!(
            note.as_deref(),
            Some("resume this session with: /bin/bkpr run claude --resume s1")
        );
    }

    #[test]
    fn safe_command_is_typed_into_its_own_pane_with_a_newline() {
        let mut typed = Vec::new();
        let note = offer_resume(
            &command(Harness::Omp, "s2", "/"),
            Some("terminal_4"),
            |pane, text| {
                typed.push((pane.to_owned(), text.to_owned()));
                Ok(())
            },
        );
        assert_eq!(note, None);
        assert_eq!(
            typed,
            [(
                "terminal_4".to_owned(),
                "/bin/bkpr run omp --resume s2\n".to_owned()
            )]
        );
    }

    /// A pty pair: (master, slave). Its slave starts in canonical mode with ECHO set.
    fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        use std::os::fd::FromRawFd;
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: valid out pointers; null name/termios/winsize are allowed.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                std::ptr::null_mut::<libc::winsize>(),
            )
        };
        assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty returned two fresh descriptors we now own.
        unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(master),
                std::os::fd::OwnedFd::from_raw_fd(slave),
            )
        }
    }

    fn local_flags(fd: &std::os::fd::OwnedFd) -> libc::tcflag_t {
        use std::os::fd::AsRawFd;
        // SAFETY: valid fd and out pointer.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(fd.as_raw_fd(), &mut termios), 0);
            termios.c_lflag
        }
    }

    #[test]
    fn echo_guard_hides_typed_input_and_restores_the_terminal_on_drop() {
        use std::os::fd::AsRawFd;
        let (master, slave) = pty();
        let before = local_flags(&slave);
        assert_ne!(before & libc::ECHO, 0);
        {
            let _guard = EchoGuard::new(slave.as_raw_fd()).expect("a pty is a terminal");
            assert_eq!(local_flags(&slave) & libc::ECHO, 0);
            assert_eq!(local_flags(&slave) & libc::ICANON, before & libc::ICANON);
            let line = b"/bin/bkpr run claude --resume s\n";
            assert_eq!(
                // SAFETY: writes from a valid buffer to our master fd.
                unsafe { libc::write(master.as_raw_fd(), line.as_ptr().cast(), line.len()) },
                line.len() as isize
            );
            assert!(wait_queued(
                slave.as_raw_fd(),
                line.len(),
                std::time::Duration::from_secs(2)
            ));
        }
        assert_eq!(local_flags(&slave), before);
        // Nothing was echoed back to the terminal, and the line is still queued for the shell.
        // SAFETY: valid fds and out pointers.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
            let mut byte = 0u8;
            assert_eq!(
                libc::read(master.as_raw_fd(), (&mut byte as *mut u8).cast(), 1),
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            let mut queued: libc::c_int = 0;
            assert_eq!(
                libc::ioctl(slave.as_raw_fd(), libc::FIONREAD, &mut queued),
                0
            );
            assert!(queued > 0);
        }
    }

    #[test]
    fn echo_guard_skips_non_terminals_and_wait_gives_up() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        assert!(EchoGuard::new(file.as_raw_fd()).is_none());
        let (_master, slave) = pty();
        let started = std::time::Instant::now();
        assert!(!wait_queued(
            slave.as_raw_fd(),
            4,
            std::time::Duration::from_millis(100)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    fn disposition(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: a null new action only queries the current one.
        unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut current), 0);
            current.sa_sigaction
        }
    }

    #[test]
    fn setup_signals_are_ignored_then_reset_to_defaults() {
        set_setup_signals(libc::SIG_IGN);
        for signal in SETUP_SIGNALS {
            assert_eq!(disposition(signal), libc::SIG_IGN, "signal {signal}");
        }
        set_setup_signals(libc::SIG_DFL);
        for signal in SETUP_SIGNALS {
            assert_eq!(disposition(signal), libc::SIG_DFL, "signal {signal}");
        }
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
    fn restore_pane_args_roundtrip_values_with_dashes_and_equals() {
        for (name, note) in [
            ("-n", "-x"),
            ("--name=evil", "--note=x"),
            ("a=b", "k=v; -/gone=1"),
            ("=", "="),
            ("--", "--"),
        ] {
            let mut recorded = pane(None, "/tmp");
            recorded.name = name.into();
            let launch = PaneLaunch::Shell {
                cwd: "/".into(),
                note: Some(note.into()),
                retry: false,
            };
            let parsed = parse_restore(restore_pane_args("w0t0p0", &recorded, &launch));
            assert_eq!(parsed.name, name);
            assert_eq!(parsed.note.as_deref(), Some(note));
            assert_eq!(parsed.cwd.as_deref(), Some("/"));
            assert!(!parsed.sticky);
        }
    }

    #[test]
    fn restore_pane_args_roundtrip_through_clap() {
        let mut recorded = pane(Some("-a"), "/srv/x");
        recorded.harness = Some("codex".into());
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
                link_harness: Some("codex".into()),
                link_agent_id: Some("-a".into()),
            }
        );
        let mut plain = pane(None, "/tmp");
        plain.name = "two words".into();
        let launch = PaneLaunch::Shell {
            cwd: "/".into(),
            note: Some("-/tmp no longer exists; \"x\"".into()),
            retry: false,
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
                link_harness: None,
                link_agent_id: None,
            }
        );
        // A refused resume carries its recorded association for a later retry.
        let mut refused = pane(Some("agent-1"), "/tmp");
        refused.harness = Some("claude".into());
        let launch = PaneLaunch::Shell {
            cwd: "/tmp".into(),
            note: Some("session is still open elsewhere".into()),
            retry: true,
        };
        let parsed = parse_restore(restore_pane_args("w0t0p0", &refused, &launch));
        assert_eq!(parsed.link_harness.as_deref(), Some("claude"));
        assert_eq!(parsed.link_agent_id.as_deref(), Some("agent-1"));
        assert_eq!(parsed.harness, None);
        assert_eq!(
            restore_link(&refused, &launch),
            Some(RestoreLink {
                harness: "claude".into(),
                agent_id: "agent-1".into(),
            })
        );
        // Without both halves of the association there is nothing to retry.
        refused.harness = None;
        assert_eq!(restore_link(&refused, &launch), None);
    }

    /// Transient refusals keep the recorded association for the next `bkpr resume`;
    /// permanent ones (no session to resume, unsupported harness) do not.
    #[test]
    fn transient_refusals_retry_later() {
        let retry = |launch: &PaneLaunch| match launch {
            PaneLaunch::Shell { retry, .. } => *retry,
            PaneLaunch::Harness { .. } => panic!("expected shell"),
        };
        let held = agents(vec![
            ("a", agent("claude", Some("s"), None, false)),
            ("b", agent("claude", Some("s"), None, true)),
        ]);
        assert!(retry(&resolve(&pane(Some("a"), "/tmp"), &held)));
        let live = agents(vec![("a", agent("omp", Some("s"), None, true))]);
        assert!(retry(&resolve(&pane(Some("a"), "/tmp"), &live)));
        let unsafe_codex = agents(vec![("a", agent("codex", Some("s"), None, true))]);
        assert!(retry(&resolve(&pane(Some("a"), "/tmp"), &unsafe_codex)));
        let uncaptured = agents(vec![("a", agent("claude", None, None, false))]);
        assert!(!retry(&resolve(&pane(Some("a"), "/tmp"), &uncaptured)));
        assert!(!retry(&resolve(&pane(Some("ghost"), "/tmp"), &uncaptured)));
        let unsupported = agents(vec![("a", agent("aider", Some("s"), None, false))]);
        assert!(!retry(&resolve(&pane(Some("a"), "/tmp"), &unsupported)));
        assert!(!retry(&resolve(&pane(None, "/tmp"), &held)));
        assert!(retry(&unresolved(&pane(Some("a"), "/tmp"), "corrupt")));
        assert!(!retry(&unresolved(&pane(None, "/tmp"), "corrupt")));
    }

    #[test]
    fn restore_name_timeout_is_reported_as_pending() {
        let timeout = anyhow::Error::new(crate::project_watch::TimedOut("zellij".into()))
            .context("send bkpr-restore-name");
        assert_eq!(
            restore_claim_note(&timeout, true).as_deref(),
            Some("pane name will apply when the session is ready")
        );
        let missing = anyhow::anyhow!("BKPR_PANE_ID is not set");
        assert!(
            restore_claim_note(&missing, true)
                .unwrap()
                .starts_with("pane name not restored: ")
        );
        // A link-only claim that timed out was still delivered: nothing to note.
        assert_eq!(restore_claim_note(&timeout, false), None);
        assert!(
            restore_claim_note(&missing, false)
                .unwrap()
                .starts_with("resume link not kept for a retry: ")
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
                note: None,
                retry: false,
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

    #[test]
    fn control_character_cwd_falls_back_like_a_missing_one() {
        // A restore layout cannot represent it, even when the directory still exists.
        let root = tempfile::tempdir().unwrap();
        let odd = root.path().join("a\u{1}b");
        fs::create_dir(&odd).unwrap();
        let (cwd, note) = existing_cwd_with(odd.to_str(), Path::new("/home/x"));
        assert_eq!(cwd, Path::new("/home/x"));
        assert!(!note.unwrap().contains('\u{1}'));
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
    fn same_owner_stale_sequence_reports_the_current_one() {
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        record_in(root.path(), &update("s1", 5)).unwrap();
        let before = fs::read(file(root.path())).unwrap();
        // A recreated owner model restarts at 1; it learns where to continue.
        let error = record_in(root.path(), &update("s1", 1)).unwrap_err();
        assert_eq!(error.downcast_ref::<StaleSequence>().unwrap().current, 5);
        // An equal sequence with different contents is a reset too, not a retry.
        let mut different: RecoveryUpdate = serde_json::from_str(&update("s1", 5)).unwrap();
        different.workspaces.clear();
        let error =
            record_in(root.path(), &serde_json::to_string(&different).unwrap()).unwrap_err();
        assert_eq!(error.downcast_ref::<StaleSequence>().unwrap().current, 5);
        assert_eq!(fs::read(file(root.path())).unwrap(), before);
        // A foreign owner is refused, never told the sequence.
        let error = record_in(root.path(), &update("s2", 1)).unwrap_err();
        assert!(error.downcast_ref::<StaleSequence>().is_none());
        assert!(error.downcast_ref::<Unclaimed>().is_none());
    }

    #[test]
    fn missing_state_root_is_unclaimed() {
        let error = record_with(Err(StateUnavailable.into()), &update("s1", 1)).unwrap_err();
        assert!(error.downcast_ref::<Unclaimed>().is_some());
    }

    #[test]
    fn record_of_another_session_name_is_refused() {
        // Guards against a session-name hash collision sharing one directory.
        let root = tempfile::tempdir().unwrap();
        claim_in(root.path(), "main", "s1", None).unwrap();
        let mut other = loaded(root.path()).unwrap();
        other.session_name = "other".into();
        write_record(&session_directory(root.path(), "main"), &other).unwrap();
        let before = fs::read(file(root.path())).unwrap();
        assert!(load_in(root.path(), "main").is_err());
        let error = record_in(root.path(), &update("s1", 1)).unwrap_err();
        assert!(error.downcast_ref::<Unclaimed>().is_none());
        assert!(claim_in(root.path(), "main", "s2", None).is_err());
        assert_eq!(fs::read(file(root.path())).unwrap(), before);
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

    /// The note must say which of the two blocking causes applies: a missing broker is not
    /// a pending-message problem, and neither is any other refusal.
    #[test]
    fn codex_stop_notes_tell_causes_apart() {
        let root = tempfile::tempdir().unwrap();
        let unavailable = crate::relay::request_as(
            &root.path().join("broker.sock"),
            "agent",
            "token",
            "deactivate",
            serde_json::json!({}),
        )
        .unwrap_err()
        .context("retire Codex broker route");
        let note = codex_stop_note(&unavailable);
        assert!(note.starts_with("relay broker unavailable"), "{note}");
        assert!(!note.contains("acknowledge"), "{note}");

        let unread =
            anyhow::anyhow!(crate::relay::UNACKNOWLEDGED_STOP).context("retire Codex broker route");
        let note = codex_stop_note(&unread);
        assert!(note.contains("unacknowledged messages"), "{note}");
        assert!(!note.contains("broker unavailable"), "{note}");

        let other = anyhow::anyhow!("Codex server process identity changed; refusing to signal it");
        let note = codex_stop_note(&other);
        assert!(
            note.starts_with("Codex session still owned by its previous server"),
            "{note}"
        );
        assert!(!note.contains("unacknowledged") && !note.contains("broker unavailable"));
    }
}
