//! Process/birth/TTY inspection adapted from main 9ad93bd crates/bkpr/src/agent.rs.
//! No environment or guessed native session IDs participate in authority.
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};
use bkpr_protocol::HarnessKind;
use std::{
    collections::HashMap,
    ffi::OsString,
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
#[cfg(target_os = "linux")]
pub(crate) use sampler::snapshot;
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
    } else {
        None
    }
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
fn recognize(executable: &Path, args: &[OsString]) -> Option<HarnessKind> {
    let name = executable_name(executable)?;
    match name {
        "node" | "nodejs" | "bun" => recognize_js(js_entrypoint(args).ok().flatten()?),
        name if python(name) => recognize_python(args).ok().flatten(),
        "cursor" => None,
        _ => HarnessKind::from_command(name),
    }
}
pub(crate) fn kind(pid: i32) -> Result<Option<HarnessKind>> {
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
            Ok(js_entrypoint(&args)?.and_then(recognize_js))
        };
    }
    Ok(recognize(&executable, &[]))
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
        match fs::read_to_string(directory.join("stat")) {
            Ok(stat) => Ok(parse_stat(pid, &stat, boot()?, include_exited)),
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
        ] {
            assert!(recognize(Path::new(command), &[]).is_some(), "{command}");
        }
        let entry = "/npm/@anthropic-ai/claude-code/cli.js";
        assert_eq!(
            recognize(Path::new("node"), &["node".into(), entry.into()]),
            Some(HarnessKind::Claude)
        );
        for args in [
            vec!["node", "--eval", entry],
            vec!["node", "--print=1", entry],
            vec!["node", "/tmp/program.js", entry],
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
