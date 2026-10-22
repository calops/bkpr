//! Git metadata is event-driven; the shared helper separately polls GitHub CI.
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::bail;

pub(crate) fn sync(json: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::sync(json)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = json;
        bail!("live Git metadata requires Linux inotify and pidfd support")
    }
}

/// Send one CLI pipe message to the plugins of the native Zellij session `zellij_session`.
pub(crate) fn send_pipe(zellij_session: &str, pipe: &str, json: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::send_pipe(zellij_session, pipe, json)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (zellij_session, pipe, json);
        bail!("native pipe delivery requires Linux process-group support")
    }
}

/// A bounded subprocess exceeded its deadline (its effect may still have happened).
#[derive(Debug)]
pub(crate) struct TimedOut(pub(crate) String);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "subprocess {} timed out", self.0)
    }
}

impl std::error::Error for TimedOut {}

pub(crate) fn serve(json: &str, listener_fd: i32) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::serve(json, listener_fd)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (json, listener_fd);
        bail!("live Git metadata requires Linux inotify and pidfd support")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::{ci, launcher, project};
    use anyhow::{Context, bail};
    use bkpr::ci_contract::CiRefreshTarget;
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::CString;
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const PIPE: &str = "bkpr-native-project";
    const LISTENER_FD: i32 = 198;

    #[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
    struct Ticket {
        generation: u64,
        owner: u32,
    }

    #[derive(Deserialize)]
    struct Request {
        server: i32,
        owner: u32,
        session: String,
        sequence: u64,
        watches: BTreeMap<String, Ticket>,
        #[serde(default)]
        refresh: Vec<CiRefreshTarget>,
    }

    #[derive(Serialize)]
    struct Update<'a> {
        cwd: &'a str,
        ticket: &'a Ticket,
        project: Option<&'a project::ProjectName>,
        error: Option<String>,
    }

    fn request(json: &str) -> Result<Request> {
        if json.len() > bkpr::MAX_MESSAGE_BYTES {
            bail!("project watch request exceeds message limit");
        }
        let request: Request = serde_json::from_str(json)?;
        launcher::validate_session(&request.session)?;
        if request.server <= 0
            || request
                .watches
                .iter()
                .any(|(cwd, ticket)| !Path::new(cwd).is_absolute() || ticket.owner != request.owner)
        {
            bail!("invalid project watch identity or cwd");
        }
        Ok(request)
    }

    pub(super) fn sync(json: &str) -> Result<()> {
        let request = request(json)?;
        let runtime = launcher::runtime_root()?.join(format!("git-{}", request.server));
        launcher::private_directory(&runtime)?;
        let _lock = launcher::startup_lock(&runtime)?;
        let socket = runtime.join("watch.sock");
        if let Ok(mut stream) = UnixStream::connect(&socket) {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            stream.write_all(json.as_bytes())?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut response = String::new();
            stream
                .take(bkpr::MAX_MESSAGE_BYTES as u64)
                .read_to_string(&mut response)?;
            if response != "ok" {
                bail!("project watcher rejected update: {response}");
            }
            return Ok(());
        }
        // Pin the actual server before spawning; a dead/reused numeric PID is not a lease.
        let server = pidfd(request.server)?;
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        let log = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(runtime.join("watch.log"))?;
        let listener_raw = listener.as_raw_fd();
        let server_raw = server.as_raw_fd();
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "project-watch-daemon",
                "--json",
                json,
                "--listener-fd",
                &LISTENER_FD.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || libc::dup2(listener_raw, LISTENER_FD) == -1
                    || libc::dup2(server_raw, LISTENER_FD + 1) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("start native Git filesystem watcher")?;
        drop(listener);
        let result = (|| -> Result<()> {
            let mut stream = UnixStream::connect(&socket)?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.write_all(json.as_bytes())?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut response = String::new();
            stream
                .take(bkpr::MAX_MESSAGE_BYTES as u64)
                .read_to_string(&mut response)?;
            if response != "ok" {
                bail!("project watcher startup failed: {response}");
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&socket);
        }
        result
    }

    fn pidfd(pid: i32) -> Result<OwnedFd> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd == -1 {
            return Err(std::io::Error::last_os_error())
                .context("watch Zellij server exit (Linux pidfd required)");
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    struct Watcher {
        fd: OwnedFd,
        // Each directory descriptor survives atomic HEAD replacement. Only metadata and
        // path-topology names trigger Git discovery; ordinary source writes do not.
        paths: BTreeMap<PathBuf, i32>,
        targets: BTreeMap<i32, BTreeMap<String, BTreeSet<Vec<u8>>>>,
    }

    impl Watcher {
        fn new() -> Result<Self> {
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd == -1 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                paths: BTreeMap::new(),
                targets: BTreeMap::new(),
            })
        }

        fn install(&mut self, directories: &BTreeMap<String, Vec<PathBuf>>) -> Result<()> {
            let mut wanted: BTreeMap<PathBuf, BTreeMap<String, BTreeSet<Vec<u8>>>> =
                BTreeMap::new();
            for (cwd, directories) in directories {
                for directory in directories {
                    if !directory.is_dir() {
                        continue;
                    }
                    let names = wanted
                        .entry(directory.clone())
                        .or_default()
                        .entry(cwd.clone())
                        .or_default();
                    for name in [
                        b".git".as_slice(),
                        b"HEAD",
                        b"commondir",
                        b"refs",
                        b"config",
                        b"config.worktree",
                        b"FETCH_HEAD",
                        b"packed-refs",
                    ] {
                        names.insert(name.to_vec());
                    }
                    // Watch children containing cwd, gitdir and loose HEAD/base refs.
                    for descendant in directories
                        .iter()
                        .map(PathBuf::as_path)
                        .chain(std::iter::once(Path::new(cwd)))
                    {
                        if let Ok(suffix) = descendant.strip_prefix(directory)
                            && let Some(child) = suffix.components().next()
                        {
                            names.insert(child.as_os_str().as_bytes().to_vec());
                        }
                    }
                    // Git metadata descendants include nested loose refs; their final filename
                    // is not a directory, so any change there must refresh the snapshot.
                    if !Path::new(cwd).starts_with(directory) {
                        names.insert(Vec::new());
                    }
                }
            }
            let removed: Vec<_> = self
                .paths
                .keys()
                .filter(|path| !wanted.contains_key(*path))
                .cloned()
                .collect();
            for path in removed {
                let descriptor = self.paths.remove(&path).unwrap();
                unsafe {
                    libc::inotify_rm_watch(self.fd.as_raw_fd(), descriptor);
                }
            }
            self.targets.clear();
            for (path, targets) in wanted {
                let descriptor = if let Some(descriptor) = self.paths.get(&path) {
                    *descriptor
                } else {
                    let path_bytes = CString::new(path.as_os_str().as_bytes())?;
                    let descriptor = unsafe {
                        libc::inotify_add_watch(
                            self.fd.as_raw_fd(),
                            path_bytes.as_ptr(),
                            libc::IN_CREATE
                                | libc::IN_MOVED_TO
                                | libc::IN_MOVED_FROM
                                | libc::IN_DELETE
                                | libc::IN_CLOSE_WRITE
                                | libc::IN_DELETE_SELF
                                | libc::IN_MOVE_SELF
                                | libc::IN_ONLYDIR,
                        )
                    };
                    if descriptor == -1 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::NotFound {
                            continue;
                        }
                        return Err(error)
                            .with_context(|| format!("watch Git directory {}", path.display()));
                    }
                    self.paths.insert(path, descriptor);
                    descriptor
                };
                self.targets.entry(descriptor).or_default().extend(targets);
            }
            Ok(())
        }

        fn changed(&mut self) -> Result<BTreeSet<String>> {
            let mut changed = BTreeSet::new();
            let mut bytes = [0u8; 16384];
            loop {
                let count = unsafe {
                    libc::read(self.fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
                };
                if count < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error.into());
                }
                if count == 0 {
                    break;
                }
                let mut offset = 0;
                while offset + std::mem::size_of::<libc::inotify_event>() <= count as usize {
                    let event = unsafe {
                        std::ptr::read_unaligned(
                            bytes.as_ptr().add(offset).cast::<libc::inotify_event>(),
                        )
                    };
                    let start = offset + std::mem::size_of::<libc::inotify_event>();
                    let end = start + event.len as usize;
                    if end > count as usize {
                        bail!("truncated inotify event");
                    }
                    let name = bytes[start..end]
                        .split(|byte| *byte == 0)
                        .next()
                        .unwrap_or_default();
                    if event.mask & libc::IN_Q_OVERFLOW != 0 {
                        changed.extend(
                            self.targets
                                .values()
                                .flat_map(|targets| targets.keys().cloned()),
                        );
                    } else if event.mask & libc::IN_IGNORED != 0 {
                        self.paths.retain(|_, descriptor| *descriptor != event.wd);
                    } else if let Some(targets) = self.targets.get(&event.wd) {
                        for (cwd, names) in targets {
                            if name.is_empty()
                                || names.contains(name)
                                || names.contains(&Vec::new())
                            {
                                changed.insert(cwd.clone());
                            }
                        }
                    }
                    if event.mask & (libc::IN_MOVE_SELF | libc::IN_DELETE_SELF) != 0 {
                        // A pathname can now name a new inode. Re-subscribe rather than
                        // retaining the descriptor of the renamed/deleted directory.
                        self.paths.retain(|_, descriptor| *descriptor != event.wd);
                        unsafe {
                            libc::inotify_rm_watch(self.fd.as_raw_fd(), event.wd);
                        }
                    }
                    offset = end;
                }
            }
            Ok(changed)
        }
    }

    fn zellij_session(session: &str) -> String {
        let identity = format!("native-local\0{session}");
        let hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        format!("bkpr-native-{hash:016x}")
    }

    fn publish(
        request: &Request,
        cwd: &str,
        project: Option<&project::ProjectName>,
        error: Option<String>,
    ) -> Result<()> {
        let update = Update {
            cwd,
            ticket: &request.watches[cwd],
            project,
            error,
        };
        let json = serde_json::to_string(&update)?;
        if json.len() > bkpr::MAX_MESSAGE_BYTES {
            bail!("project update exceeds message limit");
        }
        send_pipe(&zellij_session(&request.session), PIPE, &json)
            .context("publish native Git metadata")
    }

    pub(crate) fn send_pipe(zellij_session: &str, pipe: &str, json: &str) -> Result<()> {
        let socket_dir = launcher::zellij_socket_dir()?;
        let mut command =
            Command::new(std::env::var_os("BKPR_ZELLIJ").unwrap_or_else(|| "zellij".into()));
        command.args([
            "--session",
            zellij_session,
            "pipe",
            "--name",
            pipe,
            "--",
            json,
        ]);
        command.env("ZELLIJ_SOCKET_DIR", socket_dir);
        let output = run_output(&mut command, None, Duration::from_secs(2))?;
        check_output(&output, &format!("send {pipe}"))
    }

    fn nonblocking(fd: RawFd) -> Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
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
                    return Err(TimedOut(format!("{:?}", command.get_program())).into());
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
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::Interrupted
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
                                if bytes.len() + count > bkpr::MAX_MESSAGE_BYTES {
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
                                    if bytes.len() + count > bkpr::MAX_MESSAGE_BYTES {
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
    fn change_subscription(
        active: &Request,
        next: &Request,
        previous: &mut BTreeMap<String, (Ticket, project::ProjectName)>,
        dirty: &mut BTreeSet<String>,
    ) -> Vec<String> {
        let mut republish = Vec::new();
        for (cwd, ticket) in &next.watches {
            if active.watches.get(cwd) == Some(ticket) {
                continue;
            }
            if let Some((previous_ticket, _)) = previous.get_mut(cwd) {
                // A new owner/ticket consumes the same directory state. It is not a Git
                // event, and must not clear CI or cancel the commit's provider request.
                *previous_ticket = ticket.clone();
                republish.push(cwd.clone());
            } else {
                dirty.insert(cwd.clone());
            }
        }
        previous.retain(|cwd, _| next.watches.contains_key(cwd));
        dirty.retain(|cwd| next.watches.contains_key(cwd));
        republish
    }

    fn refresh_ci(
        active: &Request,
        incoming: &Request,
        previous: &BTreeMap<String, (Ticket, project::ProjectName)>,
        ci: &mut ci::Monitor,
    ) {
        if incoming.owner != active.owner {
            return;
        }
        for target in &incoming.refresh {
            let current = previous.iter().any(|(cwd, (ticket, project))| {
                active.watches.get(cwd) == Some(ticket)
                    && incoming.watches.get(cwd) == Some(ticket)
                    && project.ci.as_ref().is_some_and(|snapshot| {
                        snapshot.repository == target.repository
                            && !snapshot.head_sha.is_empty()
                            && snapshot.head_sha == target.head_sha
                            && project
                                .source
                                .as_ref()
                                .is_some_and(|source| source.matches_snapshot(snapshot))
                    })
            });
            if current {
                ci.refresh(target);
            }
        }
    }

    pub(super) fn serve(json: &str, listener_fd: i32) -> Result<()> {
        if listener_fd != LISTENER_FD {
            bail!("invalid project watcher listener");
        }
        let mut active = request(json)?;
        let server = unsafe { OwnedFd::from_raw_fd(LISTENER_FD + 1) };
        let listener = unsafe { UnixListener::from_raw_fd(listener_fd) };
        let socket = listener
            .local_addr()?
            .as_pathname()
            .context("watcher socket has no path")?
            .to_path_buf();
        struct SocketCleanup(PathBuf);
        impl Drop for SocketCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = SocketCleanup(socket);
        let mut watcher = Watcher::new()?;
        let mut directories = BTreeMap::new();
        let mut previous: BTreeMap<String, (Ticket, project::ProjectName)> = BTreeMap::new();
        let mut dirty: BTreeSet<_> = active.watches.keys().cloned().collect();
        let mut ci = ci::Monitor::new();
        let cache = crate::project_cache::Cache::new();
        let mut restored = Vec::new();
        let mut initialized = false;
        loop {
            if initialized && !dirty.is_empty() {
                for cwd in &dirty {
                    if let Some(source) = previous
                        .get(cwd)
                        .and_then(|(_, project)| project.source.as_ref())
                    {
                        ci.invalidate(source);
                    }
                    let cached = previous
                        .get(cwd)
                        .and_then(|(_, project)| project.ci.as_ref());
                    let paths = project::discover_watched_with_ci(Path::new(cwd), cached)
                        .map(|(_, paths)| paths)
                        .unwrap_or_else(|_| {
                            Path::new(cwd)
                                .ancestors()
                                .filter(|path| path.is_dir())
                                .map(Path::to_path_buf)
                                .collect()
                        });
                    directories.insert(cwd.clone(), paths);
                }
                watcher.install(&directories)?;
                // Discover again only after subscribing: init/checkout during setup cannot
                // leave a stale initial snapshot with no future event to repair it.
                let mut resubscribe = BTreeSet::new();
                for cwd in &dirty {
                    let cached = previous
                        .get(cwd)
                        .and_then(|(_, project)| project.ci.as_ref());
                    match project::discover_watched_with_ci(Path::new(cwd), cached) {
                        Ok((project, paths)) => {
                            if directories.get(cwd) != Some(&paths) {
                                directories.insert(cwd.clone(), paths);
                                resubscribe.insert(cwd.clone());
                                continue;
                            }
                            let mut project = project;
                            if let Some(snapshot) = project
                                .source
                                .as_ref()
                                .and_then(|source| ci.snapshot(source))
                            {
                                project::update_ci(Path::new(cwd), &mut project, Some(snapshot));
                            } else if !previous.contains_key(cwd)
                                && let Some(cached) = cache.load(cwd, &project)
                            {
                                project.branch_changes = cached.branch_changes;
                                if let (Some(source), Some(snapshot)) =
                                    (&project.source, &cached.ci)
                                {
                                    restored.push((source.clone(), snapshot.clone()));
                                }
                                project.ci = cached.ci;
                            }
                            let current = (active.watches[cwd].clone(), project);
                            if previous.get(cwd).is_some_and(|(_, prior)| {
                                prior.source == current.1.source
                                    && ((!prior.published && current.1.published)
                                        || prior.git_refs != current.1.git_refs)
                            }) && let Some(source) = &current.1.source
                            {
                                ci.published(source);
                            }
                            if previous.get(cwd) != Some(&current) {
                                if let Err(error) = cache.store(cwd, &current.1) {
                                    eprintln!("Project cache for {cwd}: {error:#}");
                                }
                                publish(&active, cwd, Some(&current.1), None)?;
                                previous.insert(cwd.clone(), current);
                            }
                        }
                        Err(error) => {
                            previous.remove(cwd);
                            publish(&active, cwd, None, Some(format!("{error:#}")))?;
                        }
                    }
                }
                dirty = resubscribe;
                if !dirty.is_empty() {
                    continue;
                }
            }
            if initialized {
                ci.sync(
                    previous
                        .values()
                        .filter_map(|(_, project)| project.source.clone()),
                );
                for (source, snapshot) in restored.drain(..) {
                    ci.restore(&source, snapshot);
                }
                // Publish repository identity while queued, before launching provider I/O.
                for (cwd, (_, project)) in &mut previous {
                    if project.ci.is_none()
                        && let Some(source) = &project.source
                        && let Some(snapshot) = ci.snapshot(source)
                    {
                        project::update_ci(Path::new(cwd), project, Some(snapshot));
                        publish(&active, cwd, Some(project), None)?;
                    }
                }
                ci.advance();
                for (cwd, (_, project)) in &mut previous {
                    let current = project
                        .source
                        .as_ref()
                        .and_then(|source| ci.snapshot(source));
                    if project::update_ci(Path::new(cwd), project, current) {
                        if let Err(error) = cache.store(cwd, project) {
                            eprintln!("Project cache for {cwd}: {error:#}");
                        }
                        publish(&active, cwd, Some(project), None)?;
                    }
                }
            }
            let mut events = [
                libc::pollfd {
                    fd: server.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: watcher.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: ci.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let result = unsafe {
                libc::poll(
                    events.as_mut_ptr(),
                    events.len() as libc::nfds_t,
                    ci.timeout(),
                )
            };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if events[0].revents != 0 {
                return Ok(());
            }
            if events[1].revents != 0 {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                let mut bytes = String::new();
                let incoming = Read::by_ref(&mut stream)
                    .take(bkpr::MAX_MESSAGE_BYTES as u64 + 1)
                    .read_to_string(&mut bytes)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| request(&bytes));
                match incoming {
                    Ok(next) if next.server == active.server && next.session == active.session => {
                        // Refresh is a control operation, not subscription replay. In
                        // particular the current sequence still carries useful requests.
                        if next.owner != active.owner || next.sequence > active.sequence {
                            let republish =
                                change_subscription(&active, &next, &mut previous, &mut dirty);
                            refresh_ci(&next, &next, &previous, &mut ci);
                            active = next;
                            directories.retain(|cwd, _| active.watches.contains_key(cwd));
                            for cwd in republish {
                                publish(&active, &cwd, Some(&previous[&cwd].1), None)?;
                            }
                            watcher.install(&directories)?;
                        } else {
                            refresh_ci(&active, &next, &previous, &mut ci);
                        }
                        stream.write_all(b"ok")?;
                        initialized = true;
                    }
                    _ => {
                        stream.write_all(b"invalid project watcher request")?;
                    }
                }
            }
            if events[2].revents != 0 {
                dirty.extend(
                    watcher
                        .changed()?
                        .into_iter()
                        .filter(|cwd| active.watches.contains_key(cwd)),
                );
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::project::tests::Repository;

        #[test]
        fn bounded_subprocess_preserves_input_output_and_exit_status() {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "cat; printf diagnostic >&2; exit 7"]);
            let output =
                run_output(&mut command, Some(b"payload"), Duration::from_secs(2)).unwrap();
            assert_eq!(output.stdout, b"payload");
            assert_eq!(output.stderr, b"diagnostic");
            assert_eq!(output.status.code(), Some(7));
            assert!(check_output(&output, "publish metadata").is_err());
        }

        #[test]
        fn bounded_subprocess_times_out_and_reaps_the_child() {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "exec sleep 30"]);
            let started = Instant::now();
            let error = run_output(&mut command, None, Duration::from_millis(50)).unwrap_err();
            assert!(error.to_string().contains("timed out"));
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn refresh_control_is_independent_of_sequence_and_never_rediscovers_or_replays_subscriptions()
         {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&[
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/project.git",
            ]);
            let cwd = repository.0.to_str().unwrap().to_owned();
            let project = project::discover(&repository.0).unwrap();
            let source = project.source.clone().unwrap();
            let ticket = Ticket {
                generation: 7,
                owner: 5,
            };
            let mut previous = BTreeMap::from([(cwd.clone(), (ticket.clone(), project))]);
            let active = Request {
                server: 1,
                owner: 5,
                session: "test".into(),
                sequence: 20,
                watches: BTreeMap::from([(cwd.clone(), ticket)]),
                refresh: Vec::new(),
            };
            let mut incoming: Request = request(
                &serde_json::json!({
                    "server": 1, "owner": 5, "session": "test", "sequence": 1,
                    "watches": active.watches,
                    "refresh": [{"repository": source.repository, "head_sha": "remote-checked-head"}],
                })
                .to_string(),
            )
            .unwrap();
            let mut monitor = ci::Monitor::new();
            monitor.sync([source.clone()].into_iter());
            let final_response = serde_json::json!({"data": {
                "r0": {"object": {"target": {"oid": "remote-checked-head",
                    "statusCheckRollup": {"contexts": {"nodes": [{
                        "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS"
                    }], "pageInfo": {"hasNextPage": false}}}
                }}, "b0": {"associatedPullRequests": {"nodes": [], "pageInfo": {"hasNextPage": false}}},
                    "c0": {"nodes": []}}, "rateLimit": {"remaining": 5000}
            }});
            monitor.finish_json_fixture(std::slice::from_ref(&source), final_response.clone());
            previous.get_mut(&cwd).unwrap().1.ci = monitor.snapshot(&source);
            assert_eq!(monitor.timeout(), -1);
            // The accepted control operation uses cached source identity, not a Git probe.
            repository.git(&["commit", "--allow-empty", "-m", "unprocessed event"]);
            refresh_ci(&active, &incoming, &previous, &mut monitor);
            assert_eq!(
                monitor.snapshot(&source).unwrap().fetch_state,
                bkpr::ci_contract::CiFetchState::Loading
            );
            assert!(previous[&cwd].0 == active.watches[&cwd]);
            assert_eq!(active.sequence, 20);
            assert_eq!(previous[&cwd].1.source.as_ref(), Some(&source));
            monitor.finish_json_fixture(std::slice::from_ref(&source), final_response.clone());
            incoming.watches.get_mut(&cwd).unwrap().generation -= 1;
            refresh_ci(&active, &incoming, &previous, &mut monitor);
            assert_eq!(monitor.timeout(), -1);
            incoming.watches = active.watches.clone();
            incoming.refresh[0].head_sha = "obsolete".into();
            refresh_ci(&active, &incoming, &previous, &mut monitor);
            assert_eq!(monitor.timeout(), -1);
            incoming.refresh[0].head_sha = "remote-checked-head".into();
            incoming.owner = 6;
            refresh_ci(&active, &incoming, &previous, &mut monitor);
            assert_eq!(monitor.timeout(), -1);
            let legacy = request(&serde_json::json!({
                "server": 1, "owner": 5, "session": "test", "sequence": 20, "watches": active.watches,
            }).to_string()).unwrap();
            assert!(legacy.refresh.is_empty());
        }

        fn watch(repository: &Repository) -> (Watcher, String) {
            let cwd = repository.0.to_str().unwrap().to_owned();
            let (_, paths) = project::discover_watched(&repository.0).unwrap();
            let mut watcher = Watcher::new().unwrap();
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            (watcher, cwd)
        }

        #[test]
        fn subscription_tickets_reuse_directory_state_without_git_or_ci_invalidation() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&[
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/project.git",
            ]);
            let (mut watcher, cwd) = watch(&repository);
            let descriptors = watcher.paths.clone();
            let initial = project::discover(&repository.0).unwrap();
            let sha = initial.source.as_ref().unwrap().sha.clone();
            let mut active = Request {
                server: 1,
                owner: 5,
                session: "test".into(),
                sequence: 1,
                watches: BTreeMap::from([(
                    cwd.clone(),
                    Ticket {
                        generation: 0,
                        owner: 5,
                    },
                )]),
                refresh: Vec::new(),
            };
            let mut previous =
                BTreeMap::from([(cwd.clone(), (active.watches[&cwd].clone(), initial))]);
            let mut dirty = BTreeSet::new();
            for owner in 6..9 {
                let next = Request {
                    server: 1,
                    owner,
                    session: "test".into(),
                    sequence: active.sequence + 1,
                    watches: BTreeMap::from([(
                        cwd.clone(),
                        Ticket {
                            generation: owner as u64,
                            owner,
                        },
                    )]),
                    refresh: Vec::new(),
                };
                assert_eq!(
                    change_subscription(&active, &next, &mut previous, &mut dirty),
                    vec![cwd.clone()],
                );
                assert!(dirty.is_empty());
                assert!(previous[&cwd].0 == next.watches[&cwd]);
                assert_eq!(previous[&cwd].1.source.as_ref().unwrap().sha, sha);
                active = next;
                let (_, paths) = project::discover_watched(&repository.0).unwrap();
                watcher
                    .install(&BTreeMap::from([(cwd.clone(), paths)]))
                    .unwrap();
                assert_eq!(watcher.paths, descriptors);
                assert!(watcher.changed().unwrap().is_empty());
            }

            repository.git(&["commit", "--allow-empty", "-m", "next"]);
            dirty.extend(watcher.changed().unwrap());
            assert!(dirty.contains(&cwd));
            assert_ne!(
                project::discover(&repository.0)
                    .unwrap()
                    .source
                    .unwrap()
                    .sha,
                sha,
            );
            let next = Request {
                server: 1,
                owner: active.owner,
                session: "test".into(),
                sequence: active.sequence + 1,
                watches: BTreeMap::new(),
                refresh: Vec::new(),
            };
            assert!(change_subscription(&active, &next, &mut previous, &mut dirty).is_empty());
            assert!(previous.is_empty());
            assert!(dirty.is_empty());
        }

        #[test]
        fn atomic_head_replacement_pushes_but_source_writes_do_not() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            let (mut watcher, cwd) = watch(&repository);
            std::fs::write(repository.0.join("source.rs"), "ordinary edit").unwrap();
            assert!(watcher.changed().unwrap().is_empty());
            let temporary = repository.0.join(".git/HEAD.lock");
            std::fs::write(&temporary, "ref: refs/heads/next\n").unwrap();
            std::fs::rename(temporary, repository.0.join(".git/HEAD")).unwrap();
            assert!(watcher.changed().unwrap().contains(&cwd));
            assert_eq!(
                project::discover(&repository.0).unwrap().branch.as_deref(),
                Some("next")
            );
        }

        #[test]
        fn git_init_is_discovered_from_parent_creation_events() {
            let repository = Repository::new();
            let (mut watcher, cwd) = watch(&repository);
            repository.git(&["init", "-b", "new"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let (_, paths) = project::discover_watched(&repository.0).unwrap();
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            repository.git(&["symbolic-ref", "HEAD", "refs/heads/changed"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
        }

        #[test]
        fn remote_config_and_remote_refs_are_event_driven_ci_invalidations() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&[
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/project.git",
            ]);
            let initial = project::discover(&repository.0).unwrap().source.unwrap();
            repository.git(&["update-ref", "refs/remotes/origin/feature/nested", "HEAD"]);
            let (mut watcher, cwd) = watch(&repository);
            repository.git(&["commit", "--allow-empty", "-m", "next"]);
            let _ = watcher.changed().unwrap();
            repository.git(&["update-ref", "refs/remotes/origin/feature/nested", "HEAD"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            repository.git(&[
                "remote",
                "set-url",
                "origin",
                "git@github.com:fixture/renamed.git",
            ]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let current = project::discover(&repository.0).unwrap().source.unwrap();
            assert_eq!(current.repository.path, "fixture/renamed");
            assert_ne!(current.sha, initial.sha);
        }

        #[test]
        fn linked_worktree_heads_are_independent_event_sources() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            let linked = repository.0.join("linked");
            repository.git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
            let root = repository.0.to_str().unwrap().to_owned();
            let cwd = linked.to_str().unwrap().to_owned();
            let mut watcher = Watcher::new().unwrap();
            watcher
                .install(&BTreeMap::from([
                    (
                        root.clone(),
                        project::discover_watched(&repository.0).unwrap().1,
                    ),
                    (cwd.clone(), project::discover_watched(&linked).unwrap().1),
                ]))
                .unwrap();
            repository.git_at(&linked, &["symbolic-ref", "HEAD", "refs/heads/next-linked"]);
            let changes = watcher.changed().unwrap();
            assert!(changes.contains(&cwd));
            assert_eq!(
                project::discover(&repository.0).unwrap().branch.as_deref(),
                Some("main")
            );
            assert_eq!(
                project::discover(&linked).unwrap().branch.as_deref(),
                Some("next-linked")
            );
        }

        #[test]
        fn local_stacked_base_advance_pushes_new_committed_totals() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&["branch", "stack/base"]);
            repository.git(&["switch", "-c", "feature"]);
            repository.write("first.rs", "first\n");
            repository.commit_all();
            let first = project::git_text(&repository.0, &["rev-parse", "HEAD"]).unwrap();
            repository.write("second.rs", "second\n");
            repository.commit_all();
            repository.git(&["config", "branch.feature.gh-merge-base", "stack/base"]);
            let (mut watcher, cwd) = watch(&repository);
            assert_eq!(
                project::discover(&repository.0)
                    .unwrap()
                    .branch_changes
                    .unwrap()
                    .total
                    .added,
                2,
            );
            repository.write("first.rs", "uncommitted source edit\n");
            assert!(watcher.changed().unwrap().is_empty());
            repository.git(&["update-ref", "refs/heads/stack/base", &first]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let changes = project::discover(&repository.0)
                .unwrap()
                .branch_changes
                .unwrap();
            assert_eq!(changes.merge_base, first);
            assert_eq!(changes.total.added, 1);
        }

        #[test]
        fn empty_new_ref_directories_and_packed_local_bases_are_watched() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&["switch", "-c", "feature"]);
            repository.write("feature.rs", "feature\n");
            repository.commit_all();
            repository.git(&["config", "branch.feature.gh-merge-base", "stack/new/base"]);
            let (mut watcher, cwd) = watch(&repository);
            assert!(
                project::discover(&repository.0)
                    .unwrap()
                    .branch_changes
                    .is_none()
            );
            std::fs::create_dir_all(repository.0.join(".git/refs/heads/stack/new")).unwrap();
            assert!(watcher.changed().unwrap().contains(&cwd));
            let (_, paths) = project::discover_watched(&repository.0).unwrap();
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            repository.git(&["update-ref", "refs/heads/stack/new/base", "main"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            assert_eq!(
                project::discover(&repository.0)
                    .unwrap()
                    .branch_changes
                    .unwrap()
                    .total
                    .added,
                1,
            );
            repository.git(&["pack-refs", "--all", "--prune"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let (_, paths) = project::discover_watched(&repository.0).unwrap();
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            repository.git(&["update-ref", "refs/heads/stack/new/base", "HEAD"]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            assert_eq!(
                project::discover(&repository.0)
                    .unwrap()
                    .branch_changes
                    .unwrap()
                    .total,
                bkpr::DiffStat::default(),
            );
        }
        #[test]
        fn linked_worktree_refs_recompute_cached_provider_targets_without_main_fallback() {
            use bkpr::ci_contract::ChangeRequestBase;
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["remote", "add", "origin", "https://github.com/owner/repo"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&["switch", "-c", "stack/parent"]);
            repository.write("parent", "parent one\nparent two\n");
            repository.commit_all();
            let parent = project::git_text(&repository.0, &["rev-parse", "HEAD"]).unwrap();
            repository.git(&["switch", "main"]);
            let linked = repository.0.join("linked");
            repository.git(&[
                "worktree",
                "add",
                "-b",
                "stack/child",
                linked.to_str().unwrap(),
                "stack/parent",
            ]);
            std::fs::write(linked.join("child"), "child only\n").unwrap();
            repository.git_at(&linked, &["add", "--all"]);
            repository.git_at(&linked, &["commit", "-m", "child"]);
            let mut project = project::discover(&linked).unwrap();
            let snapshot = project::tests::ci_snapshot(
                &project,
                ChangeRequestBase {
                    branch: "stack/parent".into(),
                    commit: parent.clone(),
                    repository: project.source.as_ref().unwrap().repository.clone(),
                },
            );
            assert!(project::update_ci(
                &linked,
                &mut project,
                Some(snapshot.clone())
            ));
            let cwd = linked.to_str().unwrap().to_owned();
            let mut watcher = Watcher::new().unwrap();
            let (_, paths) = project::discover_watched_with_ci(&linked, Some(&snapshot)).unwrap();
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            assert_eq!(project.branch_changes.as_ref().unwrap().total.added, 1);
            assert!(!project.published);
            std::fs::write(linked.join("child"), "uncommitted\nnot counted\n").unwrap();
            assert!(watcher.changed().unwrap().is_empty());

            // Simulate a target absent from the local object store, then arriving
            // with a fetched/pushed ref, without any network or Git fetch command.
            let object = repository
                .0
                .join(".git/objects")
                .join(&parent[..2])
                .join(&parent[2..]);
            let held = repository.0.join("held-parent-object");
            std::fs::rename(&object, &held).unwrap();
            let unavailable = project::discover_watched_with_ci(&linked, Some(&snapshot))
                .unwrap()
                .0;
            assert!(unavailable.branch_changes.is_none());
            assert_eq!(unavailable.ci, Some(snapshot.clone()));
            std::fs::rename(held, object).unwrap();
            repository.git(&["update-ref", "refs/remotes/origin/stack/parent", &parent]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let (restored, paths) =
                project::discover_watched_with_ci(&linked, Some(&snapshot)).unwrap();
            assert_eq!(restored.branch_changes.as_ref().unwrap().total.added, 1);
            watcher
                .install(&BTreeMap::from([(cwd.clone(), paths)]))
                .unwrap();
            let head = restored.source.as_ref().unwrap().sha.clone();
            repository.git(&["update-ref", "refs/heads/stack/parent", &head]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let current = project::discover_watched_with_ci(&linked, Some(&snapshot))
                .unwrap()
                .0;
            assert_eq!(current.branch_changes, restored.branch_changes);
            assert!(!current.published);
            repository.git(&["update-ref", "refs/remotes/origin/stack/child", &head]);
            assert!(watcher.changed().unwrap().contains(&cwd));
            let pushed = project::discover_watched_with_ci(&linked, Some(&snapshot))
                .unwrap()
                .0;
            let pushed_source = pushed.source.as_ref().unwrap();
            let prior_source = restored.source.as_ref().unwrap();
            assert_ne!(pushed_source, prior_source);
            assert_eq!(pushed_source.sha, head);
            assert_eq!(pushed.head_sha.as_deref(), Some(head.as_str()));
            assert_eq!(pushed_source.repository, prior_source.repository);
            assert_eq!(pushed_source.branch.as_deref(), Some("stack/child"));
            assert_eq!(
                pushed_source.checked_ref().as_deref(),
                Some("origin/stack/child")
            );
            assert!(
                prior_source
                    .remote_branch
                    .as_ref()
                    .unwrap()
                    .tracking_sha
                    .is_none()
            );
            assert_eq!(
                pushed_source
                    .remote_branch
                    .as_ref()
                    .unwrap()
                    .tracking_sha
                    .as_deref(),
                Some(head.as_str())
            );
            assert!(pushed.published);
            assert_ne!(pushed.git_refs, restored.git_refs);
            assert_eq!(pushed.branch_changes, restored.branch_changes);
            assert_eq!(
                pushed.branch_changes.as_ref().unwrap().base_ref,
                "stack/parent"
            );
            assert_eq!(pushed.branch_changes.as_ref().unwrap().merge_base, parent);
        }
    }
}
