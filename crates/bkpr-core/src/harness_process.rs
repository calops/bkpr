//! Process/birth/TTY inspection adapted from main 9ad93bd crates/bkpr/src/agent.rs.
//! No environment or guessed native session IDs participate in authority.
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};
use bkpr_protocol::HarnessKind;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
const MAX_PROCESSES: usize = 32768;
const MAX_ARGUMENT_BYTES: usize = 65536;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Process {
    pub pid: i32,
    pub parent: i32,
    pub group: i32,
    pub foreground: i32,
    pub tty: u64,
    pub stopped: bool,
    pub identity: String,
    #[cfg(target_os = "linux")]
    pub session: i32,
    #[cfg(target_os = "linux")]
    pub birth_ticks: u64,
}
pub(crate) use platform::{cwd, process};
mod sampler;
pub(crate) use sampler::{ObservationLease, fresh_snapshot};

pub(crate) fn lineage(
    process: &Process,
    root: &Process,
    table: &HashMap<i32, Process>,
) -> Option<Vec<Process>> {
    let mut current = process;
    let mut chain = Vec::new();
    for _ in 0..128 {
        chain.push(current.clone());
        if current.pid == root.pid {
            return (current.identity == root.identity).then_some(chain);
        }
        current = table.get(&current.parent)?;
    }
    None
}
pub(crate) fn live(chain: &[Process]) -> bool {
    chain
        .iter()
        .all(|expected| process(expected.pid).ok().flatten().as_ref() == Some(expected))
}
pub(crate) fn foreground(process: &Process, root: &Process) -> bool {
    !process.stopped
        && root.tty != 0
        && process.tty == root.tty
        && process.group == process.foreground
}
fn executable_name(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    Some(
        name.strip_prefix('.')
            .and_then(|n| n.strip_suffix("-wrapped"))
            .unwrap_or(name),
    )
}
fn python(name: &str) -> bool {
    matches!(name, "python" | "python3")
        || name
            .strip_prefix("python3.")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
fn js_entrypoint(args: &[OsString]) -> Result<Option<&str>> {
    let mut args = args.iter().skip(1);
    while let Some(argument) = args.next() {
        let arg = argument
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("indeterminate runtime argument"))?;
        match arg {
            "--" => {
                return match args.next() {
                    Some(argument) => {
                        Ok(Some(argument.to_str().ok_or_else(|| {
                            anyhow::anyhow!("indeterminate runtime entrypoint")
                        })?))
                    }
                    None => Ok(None),
                };
            }
            "--use-system-ca"
            | "--no-warnings"
            | "--trace-warnings"
            | "--trace-uncaught"
            | "--trace-deprecation"
            | "--no-deprecation"
            | "--enable-source-maps"
            | "--experimental-strip-types"
            | "--inspect"
            | "--inspect-brk"
            | "--watch"
            | "--watch-preserve-output"
            | "--no-enable-source-maps" => {}
            "--require"
            | "-r"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "--conditions"
            | "-C"
            | "--title"
            | "--env-file"
            | "--env-file-if-exists"
            | "--watch-path" => {
                if args.next().is_none() {
                    bail!("incomplete runtime argument");
                }
            }
            "--eval" | "-e" | "--print" | "-p" | "--help" | "-h" | "--version" | "-v" => {
                return Ok(None);
            }
            option if option.starts_with("--eval=") || option.starts_with("--print=") => {
                return Ok(None);
            }
            option if option.starts_with("--") && option.contains('=') => {}
            option if option.starts_with("-r") && option.len() > 2 => {}
            option if option.starts_with('-') => bail!("indeterminate runtime option arity"),
            script => return Ok(Some(script)),
        }
    }
    Ok(None)
}
fn recognize_js(script: &str) -> Option<HarnessKind> {
    if script.ends_with("/@anthropic-ai/claude-code/cli.js") {
        Some(HarnessKind::Claude)
    } else if script.ends_with("/@github/copilot/npm-loader.js") {
        Some(HarnessKind::Copilot)
    } else if script.ends_with("/cline/bin/cline") {
        Some(HarnessKind::Cline)
    } else if script.ends_with("/share/cursor-agent/index.js")
        || (Path::new(script)
            .file_name()
            .is_some_and(|n| n == "index.js")
            && Path::new(script)
                .parent()
                .and_then(Path::parent)
                .is_some_and(|p| {
                    p.file_name().is_some_and(|n| n == "versions")
                        && p.parent()
                            .and_then(Path::file_name)
                            .is_some_and(|n| n == "cursor-agent")
                }))
    {
        Some(HarnessKind::Cursor)
    } else if script.ends_with("/@earendil-works/pi-coding-agent/dist/bundle/cli.js")
        || script.ends_with("/@earendil-works/pi-coding-agent/dist/cli.js")
        || HarnessKind::from_command(script) == Some(HarnessKind::Pi)
    {
        Some(HarnessKind::Pi)
    } else {
        None
    }
}
fn recognize_js_process(args: &[OsString]) -> Result<Option<HarnessKind>> {
    // Pi's Node entrypoint replaces its argv with process.title before loading extensions.
    if matches!(args, [name] if name == "pi") {
        return Ok(Some(HarnessKind::Pi));
    }
    Ok(js_entrypoint(args)?.and_then(recognize_js))
}
fn recognize_python(args: &[OsString]) -> Result<Option<HarnessKind>> {
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        match arg
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("indeterminate Python argument"))?
        {
            "-m" => {
                let module = args
                    .next()
                    .and_then(|a| a.to_str())
                    .ok_or_else(|| anyhow::anyhow!("incomplete Python module argument"))?;
                return Ok(matches!(module, "aider" | "aider.main").then_some(HarnessKind::Aider));
            }
            "-u" | "-B" | "-E" | "-I" | "-s" | "-S" | "-P" | "-q" | "-v" | "-x" | "-O" | "-OO" => {}
            "-c" | "--help" | "-h" | "--version" | "-V" => return Ok(None),
            "-X" | "-W" => {
                if args.next().is_none() {
                    bail!("incomplete Python option");
                }
            }
            option if option.starts_with("-X") || option.starts_with("-W") => {}
            option if option.starts_with('-') && option != "--" => {
                bail!("indeterminate Python option arity")
            }
            script => {
                let script = if script == "--" {
                    args.next()
                        .and_then(|a| a.to_str())
                        .ok_or_else(|| anyhow::anyhow!("incomplete Python entrypoint"))?
                } else {
                    script
                };
                return Ok(Path::new(script)
                    .file_name()
                    .is_some_and(|n| n == "aider" || n == ".aider-wrapped")
                    .then_some(HarnessKind::Aider));
            }
        }
    }
    Ok(None)
}
fn claude_native_install(executable: &Path) -> bool {
    executable.parent().is_some_and(|versions| {
        versions.file_name().is_some_and(|name| name == "versions")
            && versions
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "claude")
    }) && executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|version| semver::Version::parse(version).is_ok())
}
fn recognize(executable: &Path, args: &[OsString]) -> Option<HarnessKind> {
    // The official installer launcher is a symlink; the kernel reports the
    // versioned native executable, not the launcher's `claude` basename.
    if claude_native_install(executable) {
        return Some(HarnessKind::Claude);
    }
    let name = executable_name(executable)?;
    match name {
        "node" | "nodejs" | "bun" => recognize_js_process(args).ok().flatten(),
        name if python(name) => recognize_python(args).ok().flatten(),
        "cursor" => None,
        _ => HarnessKind::from_command(name),
    }
}
pub(crate) fn kind(pid: i32) -> Result<Option<HarnessKind>> {
    match live_kind(pid) {
        Err(_) if process(pid)?.is_none() => Ok(None),
        result => result,
    }
}
fn live_kind(pid: i32) -> Result<Option<HarnessKind>> {
    let executable = platform::executable(pid)?;
    let name = executable_name(&executable)
        .ok_or_else(|| anyhow::anyhow!("process executable identity unavailable"))?;
    if matches!(name, "node" | "nodejs" | "bun") || python(name) {
        let args = platform::arguments(pid)?;
        if args.is_empty() {
            bail!("process arguments unavailable");
        }
        return if python(name) {
            recognize_python(&args)
        } else {
            recognize_js_process(&args)
        };
    }
    Ok(recognize(&executable, &[]))
}
/// Classify interactive harness roles separately from their native server utilities.
pub(crate) fn binding_kind(pid: i32) -> Result<Option<HarnessKind>> {
    match live_binding_kind(pid) {
        Err(_) if process(pid)?.is_none() => Ok(None),
        result => result,
    }
}
fn live_binding_kind(pid: i32) -> Result<Option<HarnessKind>> {
    let kind = kind(pid)?;
    if !matches!(kind, Some(HarnessKind::Codex | HarnessKind::Opencode)) {
        return Ok(kind);
    }
    let executable = platform::executable(pid)?;
    let args = platform::arguments(pid)?;
    let command = command_arguments(&executable, &args)?.first();
    Ok(
        match (kind, command.and_then(|argument| argument.to_str())) {
            (Some(HarnessKind::Codex), Some("app-server"))
            | (Some(HarnessKind::Opencode), Some("serve")) => None,
            _ => kind,
        },
    )
}

fn command_arguments<'a>(executable: &Path, args: &'a [OsString]) -> Result<&'a [OsString]> {
    if matches!(executable_name(executable), Some("node" | "nodejs" | "bun")) {
        let entry =
            js_entrypoint(args)?.ok_or_else(|| anyhow::anyhow!("native entrypoint unavailable"))?;
        let index = args
            .iter()
            .position(|argument| argument == entry)
            .ok_or_else(|| anyhow::anyhow!("native entrypoint argv changed"))?;
        Ok(&args[index + 1..])
    } else {
        anyhow::ensure!(!args.is_empty(), "native argv unavailable");
        Ok(&args[1..])
    }
}

/// Recognize a private OpenCode stdio server paired with its direct native owner.
pub(crate) fn owned_opencode_worker(child: &Process, owner: &Process) -> Result<bool> {
    if child.parent != owner.pid {
        return Ok(false);
    }
    let executable = platform::executable(child.pid)?;
    if executable != platform::executable(owner.pid)? {
        return Ok(false);
    }
    let child_args = platform::arguments(child.pid)?;
    let owner_args = platform::arguments(owner.pid)?;
    if recognize(&executable, &child_args) != Some(HarnessKind::Opencode)
        || recognize(&executable, &owner_args) != Some(HarnessKind::Opencode)
    {
        return Ok(false);
    }
    let command = command_arguments(&executable, &child_args)?;
    let owner = command_arguments(&executable, &owner_args)?;
    Ok(command
        .iter()
        .map(OsString::as_os_str)
        .eq(["serve", "--stdio", "--port", "0"].map(OsStr::new))
        && owner
            .iter()
            .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
            .any(|argument| argument == "--standalone" || argument == "--standalone=true"))
}
pub(crate) fn is_launcher(pid: i32, executable: &Path, kind: HarnessKind) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let expected = std::fs::metadata(executable)?;
    let actual = std::fs::metadata(platform::executable(pid)?)?;
    if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
        return Ok(false);
    }
    let args = platform::arguments(pid)?;
    let Some(first) = args.first().and_then(|s| s.to_str()) else {
        return Ok(false);
    };
    if HarnessKind::from_command(first) == Some(kind) {
        return Ok(true);
    }
    Ok(args.get(1).is_some_and(|s| s == "run")
        && args
            .get(2)
            .and_then(|s| s.to_str())
            .and_then(HarnessKind::from_command)
            == Some(kind))
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::{
        fs::{self, File},
        io::Read,
        os::unix::{ffi::OsStringExt, fs::MetadataExt},
    };
    fn parse_stat(pid: i32, stat: &str, boot: &str, include_exited: bool) -> Option<Process> {
        let (_, tail) = stat.rsplit_once(") ")?;
        let mut fields = tail.split_whitespace();
        let state = fields.next()?;
        if !include_exited && matches!(state, "Z" | "X") {
            return None;
        }
        let parent = fields.next()?.parse().ok()?;
        let group = fields.next()?.parse().ok()?;
        let session = fields.next()?.parse().ok()?;
        let tty = fields.next()?.parse::<i64>().ok()? as u64;
        let foreground = fields.next()?.parse().ok()?;
        let birth_ticks = fields.nth(13)?.parse().ok()?;
        Some(Process {
            pid,
            parent,
            group,
            foreground,
            tty,
            stopped: matches!(state, "T" | "t" | "Z" | "X"),
            identity: format!("linux:{boot}:{birth_ticks}"),
            session,
            birth_ticks,
        })
    }
    fn boot() -> Result<&'static str> {
        static BOOT: std::sync::LazyLock<std::io::Result<String>> =
            std::sync::LazyLock::new(|| {
                fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().to_owned())
            });
        BOOT.as_ref()
            .map(String::as_str)
            .map_err(|e| anyhow::anyhow!("read Linux boot identity: {e}"))
    }
    fn read_process(pid: i32, include_exited: bool) -> Result<Option<Process>> {
        if pid <= 0 {
            return Ok(None);
        }
        let directory = PathBuf::from(format!("/proc/{pid}"));
        if !fs::metadata(&directory).is_ok_and(|m| m.uid() == unsafe { libc::geteuid() }) {
            return Ok(None);
        }
        process_stat(
            pid,
            fs::read_to_string(directory.join("stat")),
            include_exited,
        )
    }
    fn process_stat(
        pid: i32,
        contents: std::io::Result<String>,
        include_exited: bool,
    ) -> Result<Option<Process>> {
        match contents {
            Ok(stat) => Ok(parse_stat(pid, &stat, boot()?, include_exited)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) || e.raw_os_error() == Some(libc::ESRCH) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn process(pid: i32) -> Result<Option<Process>> {
        read_process(pid, false)
    }
    pub fn snapshot() -> Result<HashMap<i32, Process>> {
        let mut table = HashMap::new();
        let mut count = 0;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            count += 1;
            if count > MAX_PROCESSES {
                bail!("process inspection bound exceeded");
            }
            if let Some(p) = read_process(pid, true)? {
                table.insert(pid, p);
            }
        }
        Ok(table)
    }
    pub fn executable(pid: i32) -> Result<PathBuf> {
        Ok(fs::read_link(format!("/proc/{pid}/exe"))?)
    }
    pub fn arguments(pid: i32) -> Result<Vec<OsString>> {
        let mut bytes = Vec::new();
        File::open(format!("/proc/{pid}/cmdline"))?
            .take((MAX_ARGUMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ARGUMENT_BYTES {
            bail!("process arguments exceed bound");
        }
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let arguments = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
        Ok(arguments
            .split(|b| *b == 0)
            .map(|s| OsString::from_vec(s.to_vec()))
            .collect())
    }
    pub fn cwd(pid: i32) -> Result<Option<PathBuf>> {
        match fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) => Ok(Some(path)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(test)]
    mod tests {
        #[test]
        fn exited_process_stat_is_absent_but_storage_errors_remain_failures() {
            for include_exited in [false, true] {
                for errno in [libc::ESRCH, libc::ENOENT, libc::EACCES] {
                    assert!(
                        super::process_stat(
                            1,
                            Err(std::io::Error::from_raw_os_error(errno)),
                            include_exited,
                        )
                        .unwrap()
                        .is_none()
                    );
                }
                for errno in [libc::EIO, libc::EINVAL] {
                    assert!(
                        super::process_stat(
                            1,
                            Err(std::io::Error::from_raw_os_error(errno)),
                            include_exited,
                        )
                        .is_err()
                    );
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
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
    fn check_error() -> Result<()> {
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
            check_error()?;
            return Ok(None);
        }
        if count as usize != size {
            bail!("incomplete Darwin process identity");
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
            return Err(std::io::Error::last_os_error()).context("list Darwin processes");
        }
        if bytes as usize >= capacity {
            bail!("process inspection bound exceeded");
        }
        let mut table = HashMap::new();
        for pid in pids
            .into_iter()
            .take(bytes as usize / std::mem::size_of::<i32>())
            .filter(|pid| *pid > 0)
        {
            if let Some(p) = process(pid)? {
                table.insert(pid, p);
            }
        }
        Ok(table)
    }
    pub fn executable(pid: i32) -> Result<PathBuf> {
        let mut bytes = [0_u8; 4096];
        let count = unsafe { proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if count <= 0 {
            check_error()?;
            return Ok(PathBuf::new());
        }
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        Ok(PathBuf::from(OsString::from_vec(bytes[..end].to_vec())))
    }
    pub fn arguments(pid: i32) -> Result<Vec<OsString>> {
        let mut mib = [libc::CTL_KERN, 49, pid];
        let mut bytes = vec![0_u8; MAX_ARGUMENT_BYTES];
        let mut size = bytes.len();
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            check_error()?;
            return Ok(Vec::new());
        }
        if size >= bytes.len() {
            bail!("Darwin arguments exceed inspection bound");
        }
        bytes.truncate(size);
        let argc = bytes
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .map(i32::from_ne_bytes)
            .context("invalid Darwin argc")?;
        if !(1..=4096).contains(&argc) {
            bail!("invalid Darwin argc");
        }
        let mut offset = 4
            + bytes
                .get(4..)
                .and_then(|b| b.iter().position(|b| *b == 0))
                .context("invalid Darwin executable")?
            + 1;
        while bytes.get(offset) == Some(&0) {
            offset += 1;
        }
        let mut args = Vec::new();
        for _ in 0..argc {
            let end = bytes
                .get(offset..)
                .and_then(|b| b.iter().position(|b| *b == 0))
                .context("truncated Darwin arguments")?;
            args.push(OsString::from_vec(bytes[offset..offset + end].to_vec()));
            offset += end + 1;
        }
        Ok(args)
    }
    pub fn cwd(pid: i32) -> Result<Option<PathBuf>> {
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
            check_error()?;
            return Ok(None);
        }
        if count as usize != size {
            bail!("incomplete Darwin working directory");
        }
        let end = paths
            .cwd
            .path
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(paths.cwd.path.len());
        Ok((end > 0).then(|| PathBuf::from(OsString::from_vec(paths.cwd.path[..end].to_vec()))))
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;
    pub fn process(_: i32) -> Result<Option<Process>> {
        Ok(None)
    }
    pub fn snapshot() -> Result<HashMap<i32, Process>> {
        Ok(HashMap::new())
    }
    pub fn executable(_: i32) -> Result<PathBuf> {
        bail!("verified process identity unsupported")
    }
    pub fn arguments(_: i32) -> Result<Vec<OsString>> {
        bail!("verified process identity unsupported")
    }
    pub fn cwd(_: i32) -> Result<Option<PathBuf>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn private_opencode_worker_keeps_its_native_owner_as_the_harness_boundary() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        std::fs::write(
            home.path().join("opencode-worker-fixture"),
            b"owned fixture",
        )
        .unwrap();
        // Concurrent PTY forks must not inherit a writable copied-executable FD.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "harness_process::tests::private_opencode_worker_kernel_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("HOME", home.path())
            .env("BKPR_OPENCODE_WORKER_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated native worker fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "owned copied-executable fixture; run through isolated subprocess wrapper"]
    fn private_opencode_worker_kernel_fixture() {
        use std::os::unix::fs::PermissionsExt;
        use std::{
            fs,
            io::{Read, Write},
            process::{Child, Command, Stdio},
        };
        assert_eq!(
            std::env::var("BKPR_OPENCODE_WORKER_FIXTURE").as_deref(),
            Ok("1")
        );
        let home = PathBuf::from(std::env::var_os("HOME").expect("private fixture HOME"));
        assert_eq!(
            fs::read(home.join("opencode-worker-fixture")).unwrap(),
            b"owned fixture"
        );
        struct NativeFixture(Child);
        impl Drop for NativeFixture {
            fn drop(&mut self) {
                self.0.stdin.take();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(home)
            .unwrap();
        let executable = root.path().join("opencode");
        fs::copy("/bin/sh", &executable).unwrap();
        fs::write(
            root.path().join("serve"),
            "printf '%s\\n' \"$$\"; IFS= read -r hold\n",
        )
        .unwrap();
        fs::write(
            root.path().join("parent"),
            "./opencode serve --stdio --port \"$1\" <&0 & wait \"$!\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("grand"),
            "./opencode parent 0 --standalone <&0 & wait \"$!\"\n",
        )
        .unwrap();
        for (script, flags, nested, private) in [
            ("parent", &["0", "--standalone"][..], false, true),
            ("parent", &["0"][..], false, false),
            ("parent", &["42", "--standalone"][..], false, false),
            ("grand", &["--standalone"][..], true, true),
        ] {
            let mut owned = NativeFixture(
                Command::new(&executable)
                    .arg(script)
                    .args(flags)
                    .current_dir(root.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let child = &mut owned.0;
            let mut output = child.stdout.take().unwrap();
            let mut next_pid = || {
                let mut bytes = Vec::new();
                loop {
                    let mut byte = [0];
                    output.read_exact(&mut byte).unwrap();
                    if byte[0] == b'\n' {
                        break;
                    }
                    bytes.push(byte[0]);
                }
                String::from_utf8(bytes).unwrap().parse::<i32>().unwrap()
            };
            let worker_pid = next_pid();
            let owner_pid = if nested {
                process(worker_pid).unwrap().unwrap().parent
            } else {
                child.id() as i32
            };
            let observed = (|| -> Result<()> {
                let owner = process(owner_pid)?.unwrap();
                let worker = process(worker_pid)?.unwrap();
                anyhow::ensure!(owned_opencode_worker(&worker, &owner)? == private);
                anyhow::ensure!(kind(worker_pid)? == Some(HarnessKind::Opencode));
                anyhow::ensure!(binding_kind(worker_pid)?.is_none());
                let mut chain = vec![worker, owner];
                anyhow::ensure!(
                    crate::harness::reject_nested(&chain, HarnessKind::Opencode, false).is_ok()
                        == private
                );
                anyhow::ensure!(
                    crate::harness::reject_nested(&chain, HarnessKind::Opencode, true).is_err()
                );
                anyhow::ensure!(
                    crate::harness::reject_nested(&chain, HarnessKind::Pi, false).is_err()
                );
                anyhow::ensure!(
                    crate::harness::reject_nested(&chain[..1], HarnessKind::Opencode, true)
                        .is_err()
                );
                if nested {
                    chain.push(process(child.id() as i32)?.unwrap());
                    anyhow::ensure!(
                        crate::harness::reject_nested(&chain, HarnessKind::Opencode, false)
                            .is_err()
                    );
                }
                Ok(())
            })();
            child.stdin.take().unwrap().write_all(b"done\n").unwrap();
            let status = child.wait().unwrap();
            observed.unwrap();
            assert!(status.success());
        }
    }
    #[test]
    fn recognizes_entrypoints_not_prompt_arguments() {
        for command in [
            "claude",
            "codex",
            "omp",
            "agy",
            "opencode",
            "cursor-agent",
            "copilot",
            "aider",
            "cline",
            "goose",
            "pi",
        ] {
            assert!(recognize(Path::new(command), &[]).is_some(), "{command}");
        }
        let entry = "/npm/@anthropic-ai/claude-code/cli.js";
        assert_eq!(
            recognize(Path::new("node"), &["node".into(), entry.into()]),
            Some(HarnessKind::Claude)
        );
        for args in [
            vec!["pi"],
            vec![
                "node",
                "/npm/@earendil-works/pi-coding-agent/dist/bundle/cli.js",
            ],
            vec!["node", "/launch/.pi-wrapped"],
        ] {
            assert_eq!(
                recognize(
                    Path::new("node"),
                    &args.into_iter().map(OsString::from).collect::<Vec<_>>()
                ),
                Some(HarnessKind::Pi)
            );
        }
        for args in [
            vec!["node", "--eval", entry],
            vec!["node", "--print=1", entry],
            vec!["node", "/tmp/program.js", entry],
            vec!["pi", "--eval", entry],
            vec!["node", "/tmp/program.js", "pi"],
        ] {
            assert_eq!(
                recognize(
                    Path::new("node"),
                    &args.into_iter().map(OsString::from).collect::<Vec<_>>()
                ),
                None
            );
        }
        assert_eq!(
            recognize(
                Path::new("python3.13"),
                &["python3.13".into(), "-m".into(), "aider.main".into()]
            ),
            Some(HarnessKind::Aider)
        );
    }
    #[test]
    fn recognizes_only_the_native_installer_version_layout() {
        for path in [
            "/home/owned/.local/share/claude/versions/2.1.321",
            "/custom/share/claude/versions/2.1.321-beta.1",
        ] {
            assert_eq!(recognize(Path::new(path), &[]), Some(HarnessKind::Claude));
        }
        for path in [
            "/home/owned/.local/share/other/versions/2.1.321",
            "/home/owned/.local/share/claude/versions/latest",
            "/home/owned/.local/share/claude/versions/2.1",
            "/home/owned/.local/share/claude/versions/2.1.321 (deleted)",
            "/home/owned/.local/share/claude/versions/2.1.321/tool",
            "2.1.321",
        ] {
            assert_eq!(recognize(Path::new(path), &[]), None, "{path}");
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn kernel_executable_recognizes_installer_symlink_but_not_an_unrelated_version_file() {
        use std::{fs, process::Command};
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("installer-fixture"),
            b"owned installer fixture",
        )
        .unwrap();
        let module = module_path!().split_once("::").unwrap().1;
        let fixture = format!("{module}::native_installer_kernel_fixture");
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                &fixture,
                "--test-threads=1",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", home.path())
            .env("BKPR_NATIVE_INSTALLER_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated installer fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "subprocess-only installer fixture with explicit private HOME"]
    fn native_installer_kernel_fixture() {
        use std::{
            fs,
            process::{Command, Stdio},
        };
        assert_eq!(
            std::env::var("BKPR_NATIVE_INSTALLER_FIXTURE").as_deref(),
            Ok("1")
        );
        let home = PathBuf::from(std::env::var_os("HOME").expect("private fixture HOME"));
        assert_eq!(
            fs::read(home.join("installer-fixture")).unwrap(),
            b"owned installer fixture"
        );
        // Keep copied executable writes and forks out of the parallel test host.
        let root = tempfile::tempdir_in(home).unwrap();
        for (directory, expected) in [("claude", Some(HarnessKind::Claude)), ("unrelated", None)] {
            let versions = root.path().join(directory).join("versions");
            fs::create_dir_all(&versions).unwrap();
            let executable = versions.join("2.1.321");
            fs::copy("/bin/sh", &executable).unwrap();
            let launcher = root.path().join(format!("{directory}-launcher"));
            std::os::unix::fs::symlink(&executable, &launcher).unwrap();
            let mut child = Command::new(launcher)
                .args(["-c", "IFS= read -r hold"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let observed_kind = kind(child.id() as i32);
            let observed_binding = binding_kind(child.id() as i32);
            child.kill().unwrap();
            child.wait().unwrap();
            assert_eq!(observed_kind.unwrap(), expected);
            assert_eq!(observed_binding.unwrap(), expected);
            assert_eq!(kind(child.id() as i32).unwrap(), None);
        }
    }
    #[test]
    fn lineage_binds_birth_and_foreground_tty() {
        let root = Process {
            pid: 10,
            parent: 1,
            group: 10,
            foreground: 20,
            tty: 3,
            stopped: false,
            identity: "root".into(),
            #[cfg(target_os = "linux")]
            session: 10,
            #[cfg(target_os = "linux")]
            birth_ticks: 1,
        };
        let child = Process {
            pid: 20,
            parent: 10,
            group: 20,
            foreground: 20,
            tty: 3,
            stopped: false,
            identity: "child".into(),
            #[cfg(target_os = "linux")]
            session: 10,
            #[cfg(target_os = "linux")]
            birth_ticks: 2,
        };
        let table = HashMap::from([(10, root.clone()), (20, child.clone())]);
        assert!(foreground(&child, &root));
        assert!(lineage(&child, &root, &table).is_some());
        assert!(
            lineage(
                &child,
                &Process {
                    identity: "reused".into(),
                    ..root.clone()
                },
                &table
            )
            .is_none()
        );
        assert!(!foreground(
            &Process {
                group: 99,
                ..child.clone()
            },
            &root
        ));
        assert!(!foreground(&Process { tty: 4, ..child }, &root));
    }
}

#[cfg(test)]
mod authority_argument_tests {
    use super::*;
    #[test]
    fn unknown_runtime_options_are_not_proof_of_non_harness_ancestry() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        let entry = "/npm/@anthropic-ai/claude-code/cli.js";
        assert_eq!(
            js_entrypoint(&args(&["node", "--trace-warnings", entry]))
                .unwrap()
                .and_then(recognize_js),
            Some(HarnessKind::Claude)
        );
        assert!(
            js_entrypoint(&args(&[
                "node",
                "--future-option-with-unknown-arity",
                entry
            ]))
            .is_err()
        );
        assert_eq!(
            js_entrypoint(&args(&["node", "--title", "", entry]))
                .unwrap()
                .and_then(recognize_js),
            Some(HarnessKind::Claude)
        );
        assert_eq!(
            js_entrypoint(&args(&["node", "--eval", entry])).unwrap(),
            None
        );
        assert_eq!(
            recognize_python(&args(&["python3", "-q", "-m", "aider.main"])).unwrap(),
            Some(HarnessKind::Aider)
        );
        assert!(
            recognize_python(&args(&["python3", "--future-python-option", "-m", "aider"])).is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            assert!(
                js_entrypoint(&[
                    OsString::from("node"),
                    OsString::from("--"),
                    OsString::from_vec(b"/non-utf8-\xff/@anthropic-ai/claude-code/cli.js".to_vec())
                ])
                .is_err()
            );
        }
    }
}
