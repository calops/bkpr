//! Antigravity's statusline callback, with terminal metadata kept off its display output.
use std::env;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::Deserialize;

const MAX_CALLBACK_BYTES: usize = 1024 * 1024;
const MAX_PROCESS_STAT_BYTES: u64 = 16 * 1024;
const MAX_CALLBACK_ANCESTORS: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AgentState {
    Idle,
    Thinking,
    Working,
    ToolUse,
    // Observed in the genuine 1.3.3 statusline payload during authentication.
    Authenticating,
    Initializing,
    Unknown,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Callback {
    agent_state: Option<AgentState>,
    tool_confirmation_pending: Option<bool>,
}

fn status_frame(input: &[u8]) -> Option<&'static [u8]> {
    // Only objects are statusline payloads; serde also accepts struct sequences.
    if input
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())?
        != b'{'
    {
        return None;
    }
    let callback: Callback = serde_json::from_slice(input).ok()?;
    if callback.tool_confirmation_pending == Some(true) {
        return Some(b"\x1b]7501;state=blocked:kind=permission:app=agy\x1b\\");
    }
    Some(match callback.agent_state? {
        AgentState::Idle => b"\x1b]7501;state=idle:app=agy\x1b\\",
        AgentState::Thinking | AgentState::Working | AgentState::ToolUse => {
            b"\x1b]7501;state=working:app=agy\x1b\\"
        }
        AgentState::Authenticating => b"\x1b]7501;state=blocked:kind=auth:app=agy\x1b\\",
        // OSC 7501 has no unknown state. Clear an earlier report rather than
        // inventing work, completion, or failure for initialization/new states.
        AgentState::Initializing | AgentState::Unknown | AgentState::Other => {
            b"\x1b]7501;state=clear:app=agy\x1b\\"
        }
    })
}

/// Retain the configured command's normal exit code (or shell-style 128+signal).
/// Reporting failures are deliberately silent and never bypass that command.
pub fn report(previous_command: Option<String>) -> Result<i32> {
    report_input(
        &mut io::stdin().lock(),
        previous_command.as_deref(),
        emit_status,
    )
}

fn report_input(
    input: &mut dyn Read,
    previous_command: Option<&str>,
    emit: impl FnOnce(&[u8]),
) -> Result<i32> {
    let mut prefix = Vec::new();
    let read = (&mut *input)
        .take((MAX_CALLBACK_BYTES + 1) as u64)
        .read_to_end(&mut prefix);
    if read.is_ok()
        && prefix.len() <= MAX_CALLBACK_BYTES
        && let Some(frame) = status_frame(&prefix)
    {
        emit(frame);
    }
    let Some(command) = previous_command else {
        return Ok(0);
    };
    let mut child = Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("execute configured Antigravity statusline")?;
    if let Some(mut stdin) = child.stdin.take() {
        // The metadata limit must not truncate the user's original callback.
        // No spool file contains its potentially sensitive payload, and output
        // inherits the callback's streams, so large output cannot deadlock us.
        if stdin.write_all(&prefix).is_ok() {
            // A command may intentionally stop reading or exit immediately.
            // Preserve its status rather than replacing it with BrokenPipe.
            let _ = io::copy(input, &mut stdin);
        }
    }
    let status = child.wait().context("wait for Antigravity statusline")?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessStat {
    parent: i32,
    start_time: u64,
}

fn process_stat_text(text: &str) -> Option<ProcessStat> {
    // comm is parenthesized and may itself contain spaces or closing parens.
    let mut fields = text[text.rfind(") ")? + 2..].split_ascii_whitespace();
    let state = fields.next()?;
    if state.len() != 1 || matches!(state, "Z" | "X" | "x") {
        return None;
    }
    let parent = fields.next()?.parse().ok()?;
    // Fields after comm start at state (3); starttime is field 22.
    let start_time = fields.nth(17)?.parse().ok()?;
    Some(ProcessStat { parent, start_time })
}

fn process_stat(pid: i32, uid: u32) -> Option<ProcessStat> {
    let directory = format!("/proc/{pid}");
    if fs::metadata(&directory).ok()?.uid() != uid {
        return None;
    }
    let mut input = Vec::new();
    File::open(format!("{directory}/stat"))
        .ok()?
        .take(MAX_PROCESS_STAT_BYTES + 1)
        .read_to_end(&mut input)
        .ok()?;
    if input.len() as u64 > MAX_PROCESS_STAT_BYTES {
        return None;
    }
    process_stat_text(std::str::from_utf8(&input).ok()?)
}

fn decimal_environment(name: &str) -> Option<u64> {
    let value = env::var(name).ok()?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|value| *value != 0)
}

fn same_terminal(left: &Metadata, right: &Metadata, uid: u32) -> bool {
    left.file_type().is_char_device()
        && right.file_type().is_char_device()
        && left.uid() == uid
        && right.uid() == uid
        && left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.rdev() == right.rdev()
}

fn owns_callback(
    mut pid: i32,
    root_pid: i32,
    expected_start_time: u64,
    mut read_process: impl FnMut(i32) -> Option<ProcessStat>,
) -> bool {
    for _ in 0..MAX_CALLBACK_ANCESTORS {
        let Some(process) = read_process(pid) else {
            return false;
        };
        if pid == root_pid {
            return process.start_time == expected_start_time;
        }
        if process.parent <= 1 || process.parent == pid {
            return false;
        }
        pid = process.parent;
    }
    false
}

fn emit_status(frame: &[u8]) {
    let Some(path) = env::var_os("SHEPHRD_AGY_TTY") else {
        return;
    };
    let Some(root_pid) = decimal_environment("SHEPHRD_AGY_PID")
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
    else {
        return;
    };
    let Some(start_time) = decimal_environment("SHEPHRD_AGY_START_TIME") else {
        return;
    };
    // agy-launch execs the lifecycle shell. Follow only live same-uid ancestry
    // to that birth-identified root; an intervening user executable wrapper
    // must not disable its callback. Utility/headless launches clear inherited
    // bridge variables, and SDK invocations do not produce TUI statuslines.
    let parent_pid = unsafe { libc::getppid() };
    let uid = unsafe { libc::geteuid() };
    let belongs_to_launch = || {
        owns_callback(parent_pid, root_pid, start_time, |pid| {
            process_stat(pid, uid)
        })
    };
    if !belongs_to_launch() {
        return;
    }
    let Ok(mut terminal) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(Path::new(&path))
    else {
        return;
    };
    let Ok(metadata) = terminal.metadata() else {
        return;
    };
    let Ok(root_terminal) = fs::metadata(format!("/proc/{root_pid}/fd/1")) else {
        return;
    };
    if !same_terminal(&metadata, &root_terminal, uid)
        || unsafe { libc::isatty(terminal.as_raw_fd()) } != 1
        // Recheck births and parentage immediately before writing. A detached
        // callback cannot reuse an exited root's PID or a recycled PTY name.
        || unsafe { libc::getppid() } != parent_pid
        || !belongs_to_launch()
    {
        return;
    }
    let _ = terminal.write_all(frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut template = std::env::temp_dir()
                .join("shephrd-agy-status-XXXXXX")
                .as_os_str()
                .as_bytes()
                .to_vec();
            template.push(0);
            let path = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
            assert!(!path.is_null());
            let path = unsafe { CStr::from_ptr(path) }.to_bytes();
            Self(Path::new(std::ffi::OsStr::from_bytes(path)).to_path_buf())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_actual_documented_states_report_semantic_status() {
        for (state, frame) in [
            ("idle", &b"\x1b]7501;state=idle:app=agy\x1b\\"[..]),
            ("thinking", &b"\x1b]7501;state=working:app=agy\x1b\\"[..]),
            ("working", &b"\x1b]7501;state=working:app=agy\x1b\\"[..]),
            ("tool_use", &b"\x1b]7501;state=working:app=agy\x1b\\"[..]),
            (
                "authenticating",
                &b"\x1b]7501;state=blocked:kind=auth:app=agy\x1b\\"[..],
            ),
        ] {
            let input = format!("{{\"agent_state\":\"{state}\"}}");
            assert_eq!(status_frame(input.as_bytes()), Some(frame), "{state}");
        }
    }

    #[test]
    fn permission_confirmation_precedes_every_agent_state() {
        for state in [
            "idle",
            "thinking",
            "tool_use",
            "authenticating",
            "initializing",
            "future",
        ] {
            let input =
                format!("{{\"agent_state\":\"{state}\",\"tool_confirmation_pending\":true}}");
            assert_eq!(
                status_frame(input.as_bytes()),
                Some(&b"\x1b]7501;state=blocked:kind=permission:app=agy\x1b\\"[..]),
                "{state}",
            );
        }
        assert_eq!(
            status_frame(b"{\"tool_confirmation_pending\":true}"),
            Some(&b"\x1b]7501;state=blocked:kind=permission:app=agy\x1b\\"[..]),
        );
    }

    #[test]
    fn initialization_and_unknown_states_clear_instead_of_inventing_status() {
        for state in [
            "initializing",
            "unknown",
            "new_state",
            "done",
            "error",
            "blocked",
        ] {
            let input = format!("{{\"agent_state\":\"{state}\"}}");
            assert_eq!(
                status_frame(input.as_bytes()),
                Some(&b"\x1b]7501;state=clear:app=agy\x1b\\"[..]),
                "{state}",
            );
        }
    }

    #[test]
    fn private_or_unsupported_fields_never_become_terminal_metadata() {
        let input = br#"{"agent_state":"idle","tool_confirmation_pending":null,"email":"private@example.test","prompt":"private prompt","conversation_id":"conversation","children":[{"agent_state":"working"}],"title":"private title","progress":99,"error":true}"#;
        assert_eq!(
            status_frame(input),
            Some(&b"\x1b]7501;state=idle:app=agy\x1b\\"[..]),
        );
        assert_eq!(status_frame(br#"{"prompt":"working","error":true}"#), None);
    }

    #[test]
    fn malformed_or_nonobject_payloads_do_not_report() {
        for input in [
            &b""[..],
            b"not json",
            b"{\"agent_state\":\"working\"",
            b"{\"agent_state\":true}",
            b"{\"agent_state\":\"idle\",\"tool_confirmation_pending\":\"true\"}",
            b"{\"agent_state\":\"working\",\"agent_state\":\"idle\"}",
            b"[\"idle\",false]",
            b"null",
            b"{\"agent_state\":\"idle\"} trailing",
            b"{\"agent_state\":\"\xff\"}",
        ] {
            assert_eq!(status_frame(input), None, "{input:?}");
        }
    }

    #[test]
    fn metadata_limit_accepts_exactly_the_bound_but_rejects_one_more_byte() {
        let mut input = b"{\"agent_state\":\"idle\"}".to_vec();
        input.resize(MAX_CALLBACK_BYTES, b' ');
        let mut reported = false;
        assert_eq!(
            report_input(&mut &input[..], None, |_| reported = true).unwrap(),
            0
        );
        assert!(reported);
        input.push(b' ');
        assert_eq!(
            report_input(&mut &input[..], None, |_| panic!("oversized")).unwrap(),
            0
        );
    }

    #[test]
    fn original_callback_can_exit_without_reading_input() {
        let input = vec![b'x'; MAX_CALLBACK_BYTES + 8192];
        assert_eq!(
            report_input(&mut &input[..], Some("exit 23"), |_| panic!("oversized")).unwrap(),
            23,
        );
        assert_eq!(
            report_input(&mut &b"{}"[..], Some("kill -TERM $$"), |_| {}).unwrap(),
            128 + libc::SIGTERM,
        );
    }

    fn stat(state: &str, parent: i32, birth: u64) -> String {
        format!(
            "12 (name ) with spaces) {state} {parent} {} {birth} 0\n",
            ["0"; 17].join(" ")
        )
    }

    #[test]
    fn stale_dead_or_unrelated_roots_do_not_own_callbacks() {
        let root = process_stat_text(&stat("S", 1, 100)).unwrap();
        let agy = process_stat_text(&stat("S", 42, 101)).unwrap();
        let wrapper = process_stat_text(&stat("S", 43, 102)).unwrap();
        let process = |pid| match pid {
            42 => Some(root),
            43 => Some(agy),
            44 => Some(wrapper),
            _ => None,
        };
        assert!(owns_callback(43, 42, 100, process));
        assert!(owns_callback(44, 42, 100, process));
        assert!(!owns_callback(44, 42, 99, process));
        assert!(!owns_callback(44, 45, 100, process));
        assert!(!owns_callback(44, 42, 100, |pid| {
            (pid != 42).then(|| process(pid)).flatten()
        }));
        assert_eq!(process_stat_text(&stat("Z", 1, 100)), None);
        assert_eq!(process_stat_text(&stat("X", 1, 100)), None);
        assert_eq!(process_stat_text("12 (broken) S 1"), None);
    }

    #[test]
    fn corrupt_or_excessive_ancestry_is_bounded() {
        assert!(!owns_callback(10, 42, 100, |pid| Some(ProcessStat {
            parent: pid,
            start_time: 100,
        })));
        let mut reads = 0;
        assert!(!owns_callback(10, 100, 100, |pid| {
            reads += 1;
            Some(ProcessStat {
                parent: pid + 1,
                start_time: 100,
            })
        }));
        assert_eq!(reads, MAX_CALLBACK_ANCESTORS);
    }

    #[test]
    fn recorded_terminal_identity_must_match_and_cannot_be_an_ordinary_file() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("ordinary"), b"not a tty").unwrap();
        let file = fs::metadata(fixture.0.join("ordinary")).unwrap();
        let null = fs::metadata("/dev/null").unwrap();
        let zero = fs::metadata("/dev/zero").unwrap();
        assert!(!same_terminal(&file, &file, file.uid()));
        assert!(!same_terminal(&null, &zero, null.uid()));
        assert!(!same_terminal(&null, &null, null.uid().wrapping_add(1)));
    }
}
