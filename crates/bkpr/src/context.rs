//! Authenticated, atomic native context snapshots. Readers never start a service.
use anyhow::{Context, Result, bail};
use bkpr::context_contract::{ContextPublication, PaneContext};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionBootstrap {
    pub session_id: String,
    pub session_name: String,
    pub owner_token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    server_start: String,
    publication: ContextPublication,
}

impl SessionBootstrap {
    /// Call while holding the launcher's session startup lock. A fresh native
    /// server always gets a fresh identity, including after a same-name exit.
    pub(crate) fn prepare(runtime: &Path, session_name: &str, existing: bool) -> Result<Self> {
        let runtime = Directory::open(runtime, false)?;
        let root = context_root()?;
        if existing {
            let bootstrap: Self = runtime.read("context.json")?;
            bootstrap.validate()?;
            if bootstrap.session_name != session_name {
                bail!("session context name does not match the running session");
            }
            let directory = Directory::open(&root.join(&bootstrap.session_id), false)?;
            let registered: Self = directory.read("bootstrap.json")?;
            if bootstrap != registered {
                bail!("session context bootstrap does not match its private registry");
            }
            return Ok(bootstrap);
        }
        let bootstrap = Self {
            session_id: random_hex::<24>()?,
            session_name: session_name.to_owned(),
            owner_token: random_hex::<32>()?,
        };
        bootstrap.validate()?;
        Directory::open(&root, true)?;
        let directory = Directory::open(&root.join(&bootstrap.session_id), true)?;
        directory.replace("bootstrap.json", &bootstrap)?;
        runtime.replace("context.json", &bootstrap)?;
        Ok(bootstrap)
    }

    fn validate(&self) -> Result<()> {
        validate_session_id(&self.session_id)?;
        if self.owner_token.len() != 64 || !self.owner_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid session context authority");
        }
        text(&self.session_name, false)
    }
}

fn context_root() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_support::ROOT.with(|root| root.borrow().clone()) {
        return Ok(root);
    }
    let base = if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("bkpr")
    } else {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .context("HOME or XDG_STATE_HOME is required for native context")?
            .join("bkpr/run")
    };
    Ok(base.join("contexts"))
}

fn validate_session_id(id: &str) -> Result<()> {
    if id.len() != 48 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid native session context identity");
    }
    Ok(())
}

fn text(value: &str, empty: bool) -> Result<()> {
    if (!empty && value.is_empty()) || value.len() > 4096 || value.contains('\0') {
        bail!("invalid native context label or identity");
    }
    Ok(())
}

fn validate(publication: &ContextPublication) -> Result<()> {
    validate_session_id(&publication.session_id)?;
    text(&publication.session_name, false)?;
    if publication.sequence == 0 || publication.server <= 0 {
        bail!("invalid native context sequence or server");
    }
    let mut workspaces = BTreeSet::new();
    for workspace in &publication.workspaces {
        text(&workspace.id, false)?;
        text(&workspace.name, false)?;
        if !workspaces.insert(&workspace.id) {
            bail!("duplicate native context workspace");
        }
    }
    let mut panes = BTreeSet::new();
    let mut agents = BTreeSet::new();
    for pane in &publication.panes {
        let terminal = pane
            .pane_id
            .strip_prefix("terminal_")
            .and_then(|id| id.parse::<u32>().ok());
        if terminal.is_none()
            || !panes.insert(&pane.pane_id)
            || !workspaces.contains(&pane.workspace_id)
        {
            bail!("invalid, duplicate, or unbound native context pane");
        }
        text(&pane.pane_name, true)?;
        if let Some(agent) = &pane.agent_id {
            text(agent, false)?;
            if !agents.insert(agent) {
                bail!("agent appears in more than one foreground pane");
            }
        }
    }
    Ok(())
}

pub(crate) fn publish(json: &str) -> Result<()> {
    publish_at(&context_root()?, json)
}

fn publish_at(root: &Path, json: &str) -> Result<()> {
    if json.len() > bkpr::MAX_MESSAGE_BYTES {
        bail!("native context publication exceeds message limit");
    }
    let publication: ContextPublication =
        serde_json::from_str(json).context("invalid native context publication")?;
    validate(&publication)?;
    let directory = Directory::open(&root.join(&publication.session_id), false)?;
    let _lock = directory.lock()?;
    let bootstrap: SessionBootstrap = directory.read("bootstrap.json")?;
    authenticate(&bootstrap, &publication)?;
    let server_start = process_identity(publication.server)?;
    if let Some(previous) = directory.read_optional::<Snapshot>("current.json")? {
        validate_snapshot(&bootstrap, &previous)?;
        if publication.sequence <= previous.publication.sequence {
            bail!("obsolete native context publication sequence");
        }
        // Native plugin IDs increase for the lifetime of this pinned server.
        // A delayed former owner's helper must never reclaim the projection,
        // even if it carries a larger sequence than the replacement owner.
        if publication.owner < previous.publication.owner {
            bail!("obsolete native context owner");
        }
        if previous.publication.server != publication.server
            || previous.server_start != server_start
        {
            bail!(
                "native context server incarnation changed; a fresh session identity is required"
            );
        }
    }
    // Recheck immediately before commit; a stale helper cannot revive an exited server.
    if process_identity(publication.server)? != server_start {
        bail!("native context server exited or changed during publication");
    }
    directory.require_owner_lease(publication.owner)?;
    directory.replace(
        "current.json",
        &Snapshot {
            version: 1,
            server_start,
            publication,
        },
    )
}

fn authenticate(bootstrap: &SessionBootstrap, publication: &ContextPublication) -> Result<()> {
    bootstrap.validate()?;
    if bootstrap.session_id != publication.session_id
        || bootstrap.session_name != publication.session_name
        || bootstrap.owner_token != publication.token
    {
        bail!("native context publication has foreign session authority");
    }
    Ok(())
}

fn validate_snapshot(bootstrap: &SessionBootstrap, snapshot: &Snapshot) -> Result<()> {
    if snapshot.version != 1 {
        bail!("unsupported native context snapshot version");
    }
    authenticate(bootstrap, &snapshot.publication)?;
    validate(&snapshot.publication)?;
    if process_identity(snapshot.publication.server)? != snapshot.server_start {
        bail!("native context server is no longer alive");
    }
    Ok(())
}

fn load(root: &Path, session: &str) -> Result<ContextPublication> {
    validate_session_id(session)?;
    let directory = Directory::open(&root.join(session), false)?;
    let bootstrap: SessionBootstrap = directory.read("bootstrap.json")?;
    if bootstrap.session_id != session {
        bail!("native context registry has a foreign session identity");
    }
    let snapshot: Snapshot = directory.read("current.json")?;
    validate_snapshot(&bootstrap, &snapshot)?;
    directory.require_owner_lease(snapshot.publication.owner)?;
    Ok(snapshot.publication)
}

fn pane_context(
    publication: &ContextPublication,
    pane: &bkpr::context_contract::ContextPane,
) -> PaneContext {
    let workspace = publication
        .workspaces
        .iter()
        .find(|workspace| workspace.id == pane.workspace_id)
        .expect("publication workspace references were validated");
    PaneContext {
        session_id: publication.session_id.clone(),
        session_name: publication.session_name.clone(),
        workspace_id: workspace.id.clone(),
        workspace_name: workspace.name.clone(),
        pane_id: pane.pane_id.clone(),
        pane_name: pane.pane_name.clone(),
    }
}

pub(crate) fn current() -> Result<PaneContext> {
    let session = env::var("BKPR_SESSION_ID")
        .context("missing native context: BKPR_SESSION_ID is not set")?;
    // Native Zellij's pane ID is authoritative after pane moves. Do not consult
    // inherited workspace names or BKPR_PANE_ID when a real host ID is present.
    let pane = env::var("ZELLIJ_PANE_ID")
        .or_else(|_| env::var("BKPR_PANE_ID"))
        .context("missing native context: no terminal pane identity")?;
    let pane = if pane.starts_with("terminal_") {
        pane
    } else {
        format!("terminal_{pane}")
    };
    current_at(&context_root()?, &session, &pane)
}

fn current_at(root: &Path, session: &str, pane: &str) -> Result<PaneContext> {
    let publication = load(root, session).context("native context is unavailable")?;
    current_pane(&publication, pane)
}

fn current_pane(publication: &ContextPublication, pane_id: &str) -> Result<PaneContext> {
    let pane = publication
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("native context is unavailable: pane is no longer in the live session")?;
    Ok(pane_context(publication, pane))
}

/// One scan per broker operation. Invalid/dead sessions contribute no attached
/// members; conflicting agent identities are omitted rather than guessed.
pub(crate) fn agent_contexts() -> Result<BTreeMap<String, PaneContext>> {
    agent_contexts_at(&context_root()?)
}

fn agent_contexts_at(root: &Path) -> Result<BTreeMap<String, PaneContext>> {
    let directory = match Directory::open(root, false) {
        Ok(directory) => directory,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(error),
    };
    let mut result = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    // Directory enumeration carries no authority; each candidate is reopened
    // using no-follow traversal and validated against its own bootstrap.
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let Some(session) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(publication) = load(root, &session) else {
            continue;
        };
        for pane in &publication.panes {
            if let Some(agent) = &pane.agent_id
                && result
                    .insert(agent.clone(), pane_context(&publication, pane))
                    .is_some()
            {
                ambiguous.insert(agent.clone());
            }
        }
    }
    drop(directory);
    for agent in ambiguous {
        result.remove(&agent);
    }
    Ok(result)
}

fn random_hex<const N: usize>() -> Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut random = [0; N];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let mut result = String::with_capacity(N * 2);
    for byte in random {
        result.push(HEX[usize::from(byte >> 4)] as char);
        result.push(HEX[usize::from(byte & 15)] as char);
    }
    Ok(result)
}

/// All component traversal is no-follow. File operations stay anchored to the
/// opened directory, including atomic replacement and lock acquisition.
struct Directory(File);

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Self> {
        if !path.is_absolute() {
            bail!("native context paths must be absolute");
        }
        let mut directory = File::open("/")?;
        for component in path.components() {
            let Component::Normal(name) = component else {
                if component == Component::RootDir {
                    continue;
                }
                bail!("unsafe native context path component");
            };
            let name = CString::new(name.as_bytes())?;
            if create && unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
            }
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            directory = unsafe { File::from_raw_fd(fd) };
        }
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            bail!("native context requires a private current-user-owned directory");
        }
        Ok(Self(directory))
    }

    fn file(&self, name: &str, flags: i32) -> Result<File> {
        let name = CString::new(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            bail!("unsafe native context file type, ownership, permissions, or hard links");
        }
        Ok(file)
    }

    fn read<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let file = self.file(name, libc::O_RDONLY)?;
        if file.metadata()?.len() > bkpr::MAX_MESSAGE_BYTES as u64 {
            bail!("native context file exceeds message limit");
        }
        let mut bytes = Vec::new();
        file.take(bkpr::MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > bkpr::MAX_MESSAGE_BYTES {
            bail!("native context file exceeds message limit");
        }
        serde_json::from_slice(&bytes).context("invalid private native context file")
    }

    fn read_optional<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match self.read(name) {
            Ok(value) => Ok(Some(value)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn lock(&self) -> Result<File> {
        let file = self.file("publish.lock", libc::O_RDWR | libc::O_CREAT)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(file)
    }

    fn require_owner_lease(&self, owner: u32) -> Result<()> {
        let file = self.file(&format!("owner-{owner}.lock"), libc::O_RDWR)?;
        if file.metadata()?.mode() & 0o7777 != 0o600 {
            bail!("unsafe native context owner lease permissions");
        }
        // Only the native owner's independently held lock proves liveness.
        // A leftover file, or a lock we can acquire ourselves, grants nothing.
        match file.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => Ok(()),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
            Ok(()) => {
                file.unlock()?;
                bail!("native context owner is no longer alive");
            }
        }
    }

    fn replace<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        // Refuse unsafe existing destinations instead of silently healing them.
        match self.file(name, libc::O_RDONLY) {
            Ok(_) => {}
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > bkpr::MAX_MESSAGE_BYTES {
            bail!("native context snapshot exceeds message limit");
        }
        let temporary = format!(".context-{}", random_hex::<12>()?);
        let result = (|| -> Result<()> {
            let mut file = self.file(&temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            let from = CString::new(temporary.as_str())?;
            let to = CString::new(name)?;
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    from.as_ptr(),
                    self.0.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.0.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let name = CString::new(temporary)?;
            unsafe {
                libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        result
    }
}

#[cfg(target_os = "linux")]
fn process_identity(pid: i32) -> Result<String> {
    if pid <= 0 {
        bail!("invalid native server PID");
    }
    let directory = PathBuf::from(format!("/proc/{pid}"));
    if std::fs::metadata(&directory)?.uid() != unsafe { libc::geteuid() } {
        bail!("native context server belongs to another user");
    }
    let stat = std::fs::read_to_string(directory.join("stat"))?;
    let (_, fields) = stat
        .rsplit_once(") ")
        .context("invalid native server process stat")?;
    let mut fields = fields.split_whitespace();
    let state = fields
        .next()
        .context("missing native server process state")?;
    if matches!(state, "Z" | "X" | "x") {
        bail!("native context server is no longer alive");
    }
    let start = fields
        .nth(18)
        .context("missing native server process start")?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(format!("linux:{}:{start}", boot.trim()))
}

#[cfg(target_os = "macos")]
fn process_identity(pid: i32) -> Result<String> {
    // PROC_PIDTBSDINFO's stable public ABI, including microsecond birth time.
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
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            bytes: i32,
        ) -> i32;
    }
    if pid <= 0 {
        bail!("invalid native server PID");
    }
    let mut info: BsdInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<BsdInfo>();
    let count = unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut BsdInfo).cast(), size as i32) };
    if count != size as i32
        || info.uid != unsafe { libc::geteuid() }
        || info.status == 5
        || info.flags & 4 != 0
    {
        bail!("native context server is unavailable");
    }
    Ok(format!("darwin:{}:{}", info.start_sec, info.start_usec))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_identity(_pid: i32) -> Result<String> {
    bail!("live native context is unsupported on this operating system")
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use bkpr::context_contract::{ContextPane, ContextWorkspace};
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    thread_local! {
        pub(super) static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(crate) struct Guard(Option<PathBuf>);

    impl Drop for Guard {
        fn drop(&mut self) {
            ROOT.with(|root| *root.borrow_mut() = self.0.take());
        }
    }

    pub(crate) struct Session {
        pub(super) owner_lease: Option<File>,
        // Keep each fixture's real runtime alive without modifying process env.
        pub(super) _runtime: tempfile::TempDir,
        root: PathBuf,
        pub(super) bootstrap: SessionBootstrap,
    }

    impl Session {
        pub(crate) fn new(name: &str) -> Result<Self> {
            let runtime = tempfile::tempdir()?;
            let root = runtime.path().join("contexts");
            Self::create(runtime, root, name)
        }

        pub(crate) fn new_in(root: &Path, name: &str) -> Result<Self> {
            Self::create(tempfile::tempdir()?, root.to_owned(), name)
        }

        fn create(runtime: tempfile::TempDir, root: PathBuf, name: &str) -> Result<Self> {
            std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))?;
            let previous = ROOT.with(|current| current.replace(Some(root.clone())));
            let guard = Guard(previous);
            let bootstrap = SessionBootstrap::prepare(runtime.path(), name, false)?;
            drop(guard);
            let mut session = Self {
                owner_lease: None,
                _runtime: runtime,
                root,
                bootstrap,
            };
            session.owner_lease = Some(session.lock_owner(1)?);
            Ok(session)
        }

        pub(crate) fn root(&self) -> &Path {
            &self.root
        }
        pub(crate) fn session_id(&self) -> &str {
            &self.bootstrap.session_id
        }

        pub(crate) fn guard(&self) -> Guard {
            Guard(ROOT.with(|root| root.replace(Some(self.root.clone()))))
        }

        pub(super) fn lock_owner(&self, owner: u32) -> Result<File> {
            let directory = Directory::open(&self.root.join(self.session_id()), false)?;
            let file =
                directory.file(&format!("owner-{owner}.lock"), libc::O_RDWR | libc::O_CREAT)?;
            file.try_lock()?;
            Ok(file)
        }

        pub(crate) fn publish(
            &self,
            sequence: u64,
            workspaces: Vec<ContextWorkspace>,
            panes: Vec<ContextPane>,
        ) -> Result<()> {
            self.publish_server(sequence, std::process::id() as i32, workspaces, panes)
        }

        pub(super) fn publish_server(
            &self,
            sequence: u64,
            server: i32,
            workspaces: Vec<ContextWorkspace>,
            panes: Vec<ContextPane>,
        ) -> Result<()> {
            let publication = ContextPublication {
                session_id: self.bootstrap.session_id.clone(),
                session_name: self.bootstrap.session_name.clone(),
                server,
                owner: 1,
                sequence,
                token: self.bootstrap.owner_token.clone(),
                workspaces,
                panes,
            };
            publish_at(&self.root, &serde_json::to_string(&publication)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::context_contract::{ContextPane, ContextWorkspace};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use test_support::Session;

    fn workspace(id: &str, name: &str) -> ContextWorkspace {
        ContextWorkspace {
            id: id.into(),
            name: name.into(),
        }
    }

    fn pane(workspace: &str, agent: Option<&str>) -> ContextPane {
        ContextPane {
            pane_id: "terminal_3".into(),
            workspace_id: workspace.into(),
            pane_name: "Editor".into(),
            agent_id: agent.map(str::to_owned),
        }
    }

    #[test]
    fn latest_projection_moves_renames_and_closes_actual_panes() -> Result<()> {
        let session = Session::new("Work")?;
        let _guard = session.guard();
        session.publish(
            1,
            vec![workspace("a", "Old")],
            vec![pane("a", Some("agent"))],
        )?;
        let first = agent_contexts()?.remove("agent").unwrap();
        assert_eq!(first.workspace_id, "a");
        session.publish(
            2,
            vec![workspace("b", "Renamed")],
            vec![pane("b", Some("agent"))],
        )?;
        let projection = load(session.root(), session.session_id())?;
        let current = current_pane(&projection, "terminal_3")?;
        assert_eq!(current.workspace_id, "b");
        assert_eq!(current.workspace_name, "Renamed");
        assert_eq!(current.pane_name, "Editor");
        assert_eq!(agent_contexts()?.get("agent"), Some(&current));
        let json = serde_json::to_value(current)?;
        assert!(json.get("token").is_none());
        session.publish(3, vec![], vec![])?;
        assert!(agent_contexts()?.is_empty());
        assert!(current_pane(&load(session.root(), session.session_id())?, "terminal_3").is_err());
        Ok(())
    }

    #[test]
    fn invalid_foreign_partial_and_reordered_frames_preserve_committed_state() -> Result<()> {
        let session = Session::new("Work")?;
        session.publish(
            8,
            vec![workspace("a", "Good")],
            vec![pane("a", Some("agent"))],
        )?;
        let original = load(session.root(), session.session_id())?;
        let mut bad = original.clone();
        bad.sequence = 9;
        bad.token = "foreign".into();
        assert!(publish_at(session.root(), &serde_json::to_string(&bad)?).is_err());
        bad = original.clone();
        bad.sequence = 9;
        bad.panes[0].workspace_id = "missing".into();
        assert!(publish_at(session.root(), &serde_json::to_string(&bad)?).is_err());
        bad = original.clone();
        bad.sequence = 9;
        bad.session_name = "Foreign".into();
        assert!(publish_at(session.root(), &serde_json::to_string(&bad)?).is_err());
        assert!(publish_at(session.root(), "{\"session_id\":").is_err());
        assert!(session.publish(8, vec![], vec![]).is_err());
        assert!(session.publish(7, vec![], vec![]).is_err());
        let oversized = " ".repeat(bkpr::MAX_MESSAGE_BYTES + 1);
        assert!(publish_at(session.root(), &oversized).is_err());
        assert_eq!(load(session.root(), session.session_id())?, original);
        // A plugin reload may change owner, but only with a newer reservation.
        let mut replacement = original.clone();
        replacement.owner = 42;
        replacement.sequence = 10;
        let _replacement_lease = session.lock_owner(replacement.owner)?;
        publish_at(session.root(), &serde_json::to_string(&replacement)?)?;
        assert!(publish_at(session.root(), &serde_json::to_string(&original)?).is_err());
        let mut obsolete_owner = original.clone();
        obsolete_owner.sequence = 11;
        assert!(publish_at(session.root(), &serde_json::to_string(&obsolete_owner)?).is_err());
        assert_eq!(load(session.root(), session.session_id())?, replacement);
        Ok(())
    }

    #[test]
    fn competing_helpers_cannot_roll_back_a_newer_committed_sequence() -> Result<()> {
        let session = std::sync::Arc::new(Session::new("Work")?);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let mut helpers = Vec::new();
        for sequence in 1..=16 {
            let session = session.clone();
            let barrier = barrier.clone();
            helpers.push(std::thread::spawn(move || {
                barrier.wait();
                let result = session.publish(
                    sequence,
                    vec![workspace("a", &sequence.to_string())],
                    vec![],
                );
                (sequence, result.is_ok())
            }));
        }
        let mut latest_succeeded = false;
        for helper in helpers {
            let (sequence, succeeded) = helper.join().expect("publication helper panicked");
            if sequence == 16 {
                latest_succeeded = succeeded;
            }
        }
        assert!(latest_succeeded);
        let latest = load(session.root(), session.session_id())?;
        assert_eq!(latest.sequence, 16);
        assert_eq!(latest.workspaces[0].name, "16");
        Ok(())
    }

    #[test]
    fn owner_lease_drop_invalidates_live_server_context_and_delayed_publications() -> Result<()> {
        let mut session = Session::new("Work")?;
        let _guard = session.guard();
        session.publish(1, vec![workspace("a", "A")], vec![pane("a", Some("agent"))])?;
        let original = load(session.root(), session.session_id())?;
        let server_start = process_identity(original.server)?;
        assert!(current_at(session.root(), session.session_id(), "terminal_3").is_ok());
        assert!(agent_contexts()?.contains_key("agent"));

        drop(session.owner_lease.take());
        assert_eq!(process_identity(original.server)?, server_start);
        assert!(current_at(session.root(), session.session_id(), "terminal_3").is_err());
        assert!(agent_contexts()?.is_empty());
        let directory = Directory::open(&session.root().join(session.session_id()), false)?;
        assert!(directory.file("owner-1.lock", libc::O_RDWR).is_ok());

        let mut delayed = original.clone();
        delayed.sequence = 2;
        assert!(publish_at(session.root(), &serde_json::to_string(&delayed)?).is_err());
        let committed: Snapshot = directory.read("current.json")?;
        assert_eq!(committed.publication, original);

        // Replacing an unloaded owner must not require its dead lease to live on.
        let mut replacement = original.clone();
        replacement.owner = 42;
        replacement.sequence = 3;
        let _replacement_lease = session.lock_owner(replacement.owner)?;
        publish_at(session.root(), &serde_json::to_string(&replacement)?)?;
        assert!(current_at(session.root(), session.session_id(), "terminal_3").is_ok());
        assert!(agent_contexts()?.contains_key("agent"));

        // Even if the old owner reacquires its file, its late helper is obsolete.
        let _obsolete_lease = session.lock_owner(original.owner)?;
        delayed.sequence = 4;
        assert!(publish_at(session.root(), &serde_json::to_string(&delayed)?).is_err());
        assert_eq!(load(session.root(), session.session_id())?, replacement);
        Ok(())
    }

    #[test]
    fn owner_lease_requires_an_existing_private_regular_unaliased_file() -> Result<()> {
        let session = Session::new("Work")?;
        session.publish(1, vec![workspace("a", "A")], vec![pane("a", Some("agent"))])?;
        let directory = session.root().join(session.session_id());
        let lease = directory.join("owner-1.lock");
        std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o644))?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());
        std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o600))?;

        let alias = directory.join("lease-alias");
        std::fs::hard_link(&lease, &alias)?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());
        std::fs::remove_file(&alias)?;
        std::fs::rename(&lease, &alias)?;
        symlink(&alias, &lease)?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());
        std::fs::remove_file(&lease)?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());

        // A different, unlocked inode at the same trusted path is not authority.
        let native = Directory::open(&directory, false)?;
        drop(native.file("owner-1.lock", libc::O_RDWR | libc::O_CREAT)?);
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());
        std::fs::remove_file(&lease)?;
        std::fs::rename(&alias, &lease)?;
        assert!(load(session.root(), session.session_id()).is_ok());
        Ok(())
    }

    #[test]
    fn exited_server_never_remains_attached_even_with_snapshot_and_bootstrap() -> Result<()> {
        let session = Session::new("Work")?;
        let mut child = std::process::Command::new("sleep").arg("60").spawn()?;
        let published = session.publish_server(
            1,
            child.id() as i32,
            vec![workspace("a", "A")],
            vec![pane("a", Some("agent"))],
        );
        let killed = child.kill();
        let waited = child.wait();
        published?;
        killed?;
        waited?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(agent_contexts_at(session.root())?.is_empty());
        assert!(session.publish(2, vec![], vec![]).is_err());
        assert!(
            session
                .root()
                .join(session.session_id())
                .join("current.json")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn server_birth_identity_is_checked_not_just_pid_liveness() -> Result<()> {
        let session = Session::new("Work")?;
        session.publish(1, vec![workspace("a", "A")], vec![pane("a", Some("agent"))])?;
        let directory = Directory::open(&session.root().join(session.session_id()), false)?;
        let mut snapshot: Snapshot = directory.read("current.json")?;
        snapshot.server_start.push_str("-old-incarnation");
        directory.replace("current.json", &snapshot)?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(agent_contexts_at(session.root())?.is_empty());
        assert!(session.publish(2, vec![], vec![]).is_err());
        Ok(())
    }

    #[test]
    fn private_files_reject_symlinks_hardlinks_public_modes_and_unsafe_directories() -> Result<()> {
        let session = Session::new("Work")?;
        session.publish(1, vec![], vec![])?;
        let path = session
            .root()
            .join(session.session_id())
            .join("current.json");
        let saved = path.with_file_name("saved.json");
        std::fs::rename(&path, &saved)?;
        symlink(&saved, &path)?;
        assert!(load(session.root(), session.session_id()).is_err());
        assert!(session.publish(2, vec![], vec![]).is_err());
        std::fs::remove_file(&path)?;
        std::fs::hard_link(&saved, &path)?;
        assert!(load(session.root(), session.session_id()).is_err());
        std::fs::remove_file(&path)?;
        std::fs::rename(&saved, &path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(load(session.root(), session.session_id()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let private = path.parent().unwrap();
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o755))?;
        assert!(load(session.root(), session.session_id()).is_err());
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
        let alias = session.root().join("alias");
        symlink(private, &alias)?;
        assert!(Directory::open(&alias, false).is_err());
        assert!(Directory::open(&alias.join("child"), true).is_err());
        Ok(())
    }

    #[test]
    fn live_sessions_are_batched_and_ambiguous_membership_fails_closed() -> Result<()> {
        let first = Session::new("First")?;
        let second = Session::new_in(first.root(), "Second")?;
        first.publish(1, vec![workspace("a", "A")], vec![pane("a", Some("first"))])?;
        second.publish(
            1,
            vec![workspace("b", "B")],
            vec![pane("b", Some("second"))],
        )?;
        let contexts = agent_contexts_at(first.root())?;
        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts["second"].session_id, second.session_id());
        second.publish(2, vec![workspace("b", "B")], vec![pane("b", Some("first"))])?;
        assert!(agent_contexts_at(first.root())?.is_empty());
        let malformed = Directory::open(&first.root().join(second.session_id()), false)?;
        malformed.replace("current.json", &serde_json::json!({"incomplete":true}))?;
        let contexts = agent_contexts_at(first.root())?;
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts["first"].session_id, first.session_id());
        Ok(())
    }

    #[test]
    fn bootstrap_reuses_only_verified_existing_identity_and_fresh_sessions_rotate() -> Result<()> {
        let session = Session::new("Work")?;
        let _guard = session.guard();
        let runtime = session._runtime.path();
        let existing = SessionBootstrap::prepare(runtime, "Work", true)?;
        assert_eq!(existing.session_id, session.session_id());
        assert_eq!(existing.owner_token, session.bootstrap.owner_token);
        assert!(SessionBootstrap::prepare(runtime, "Foreign", true).is_err());
        let fresh = SessionBootstrap::prepare(runtime, "Work", false)?;
        assert_ne!(existing.session_id, fresh.session_id);
        assert_ne!(existing.owner_token, fresh.owner_token);
        Ok(())
    }
}
