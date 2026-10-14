//! Automatic Antigravity status callbacks; authentication and history stay with agy.
use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
const BRIDGE_MARKER: &str = "\n# shephrd-agy-status:";
const BRIDGE_VERSION: &str = "v1:";
const SETTINGS_NAME: &str = "settings.json";
const LOCK_NAME: &str = ".shephrd-statusline.lock";
const CONTEXT_VARIABLES: [&str; 3] = [
    "SHEPHRD_AGY_TTY",
    "SHEPHRD_AGY_PID",
    "SHEPHRD_AGY_START_TIME",
];
const SESSION_SCRIPT: &str = r#"trap 'printf "\033]7501;state=clear\033\134"' EXIT; "$@""#;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

pub fn wrapper(program: &Path, directory: &Path, name: &str) -> Result<String> {
    validate_name(name)?;
    let program = program
        .to_str()
        .context("Antigravity wrapper program path is not UTF-8")?;
    let directory = directory
        .to_str()
        .context("Antigravity wrapper directory is not UTF-8")?;
    Ok(format!(
        "#!/bin/sh\nexec {} agy-launch --wrapper-dir {} --name {} -- \"$@\"\n",
        shell_quote(program),
        shell_quote(directory),
        shell_quote(name),
    ))
}

pub fn launch(directory: &Path, name: &str, arguments: Vec<OsString>) -> Result<()> {
    validate_name(name)?;
    let executable = real_executable(directory, name)?;
    let mut command = Command::new(&executable);
    for variable in CONTEXT_VARIABLES {
        command.env_remove(variable);
    }

    if managed_wrapper(directory)
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && interactive_arguments(&arguments)
    {
        match prepare_reporting() {
            Ok((terminal, start_time)) => {
                // The shell keeps the launch PID and controlling terminal. Its
                // foreground agy child owns the callback; the EXIT trap also
                // clears idle metadata, which native shell-prompt expiry retains.
                command = Command::new("/bin/sh");
                command
                    .arg("-c")
                    .arg(SESSION_SCRIPT)
                    .arg("shephrd-agy")
                    .arg(&executable)
                    .env("SHEPHRD_AGY_TTY", terminal)
                    .env("SHEPHRD_AGY_PID", std::process::id().to_string())
                    .env("SHEPHRD_AGY_START_TIME", start_time);
            }
            Err(error) => {
                eprintln!("shephrd: Antigravity status reporting unavailable: {error:#}");
            }
        }
    }
    Err(command.args(arguments).exec()).context("execute Antigravity")
}

fn validate_name(name: &str) -> Result<()> {
    if !matches!(name, "agy" | "antigravity") {
        bail!("unsupported Antigravity executable name");
    }
    Ok(())
}

fn managed_wrapper(directory: &Path) -> bool {
    let Some(configured) = env::var_os("SHEPHRD_HARNESS_WRAPPER_DIR") else {
        return false;
    };
    match (fs::canonicalize(configured), fs::canonicalize(directory)) {
        (Ok(configured), Ok(directory)) => configured == directory,
        _ => false,
    }
}

fn real_executable(directory: &Path, name: &str) -> Result<PathBuf> {
    resolve_executable(
        directory,
        name,
        &env::var_os("PATH").unwrap_or_default(),
        &env::current_dir()?,
    )
}

fn resolve_executable(
    directory: &Path,
    name: &str,
    path: &OsStr,
    current: &Path,
) -> Result<PathBuf> {
    let directory =
        fs::canonicalize(directory).context("locate Antigravity session wrapper directory")?;
    for entry in env::split_paths(path) {
        let candidate = entry.join(name);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            current.join(candidate)
        };
        if fs::metadata(&candidate)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            && let Ok(resolved) = fs::canonicalize(&candidate)
            && !resolved.starts_with(&directory)
        {
            // Keep the original symlink/wrapper entry point, but make it absolute
            // so relative and empty PATH entries cannot recurse through PATH.
            return Ok(candidate);
        }
    }
    bail!("Antigravity is not installed on PATH outside the session wrapper")
}

fn interactive_arguments(arguments: &[OsString]) -> bool {
    let mut index = 0;
    let mut first_positional = true;
    while index < arguments.len() {
        let Some(argument) = arguments[index].to_str() else {
            return false;
        };
        if argument == "--" {
            return true;
        }
        let (option, value) = argument
            .split_once('=')
            .map_or((argument, None), |(option, value)| (option, Some(value)));
        match option {
            "--print"
            | "-p"
            | "--prompt"
            | "--help"
            | "-h"
            | "--version"
            | "-v"
            | "--input-format"
            | "--output-format"
            | "--json-schema"
            | "--print-timeout"
            | "--disable-slash-commands" => return false,
            "--add-dir"
            | "--agent"
            | "--conversation"
            | "--effort"
            | "--log-file"
            | "--mode"
            | "--model"
            | "--project"
            | "--prompt-interactive"
            | "-i" => {
                if value.is_none() {
                    index += 1;
                    if arguments.get(index).is_none() {
                        return false;
                    }
                }
            }
            "--continue"
            | "-c"
            | "--dangerously-skip-permissions"
            | "--remote-control"
            | "--sandbox"
                if value.is_none() || matches!(value, Some("true" | "false")) => {}
            _ if option.starts_with('-') => {
                // Unknown flags may take values, including print/help-looking
                // values. Preserve agy's own parsing rather than guessing.
                return false;
            }
            _ => {
                if first_positional
                    && matches!(
                        argument,
                        "agent"
                            | "agents"
                            | "changelog"
                            | "help"
                            | "install"
                            | "mcp"
                            | "mic-serve"
                            | "models"
                            | "plugin"
                            | "plugins"
                            | "remote-control"
                            | "update"
                    )
                {
                    return false;
                }
                first_positional = false;
            }
        }
        index += 1;
    }
    true
}

fn prepare_reporting() -> Result<(OsString, String)> {
    let terminal = terminal_path()?;
    let start_time = launch_start_time()?;
    let program = env::current_exe().context("locate Antigravity status callback")?;
    let home = env::var_os("HOME").context("HOME is unset")?;
    update_settings(Path::new(&home), &program)?;
    Ok((terminal, start_time))
}

fn terminal_path() -> Result<OsString> {
    let mut buffer = [0 as libc::c_char; 4096];
    let result = unsafe { libc::ttyname_r(libc::STDOUT_FILENO, buffer.as_mut_ptr(), buffer.len()) };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result))
            .context("locate Antigravity launch terminal");
    }
    let bytes = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes();
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

fn launch_start_time() -> Result<String> {
    let stat = fs::read_to_string("/proc/self/stat").context("read Antigravity launch identity")?;
    let value = stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .context("Antigravity launch identity has no start time")?;
    value
        .parse::<u64>()
        .context("Antigravity launch start time is invalid")?;
    Ok(value.to_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bridge {
    program: String,
    previous_command: Option<String>,
}

fn callback_command(program: &str, previous_command: Option<&str>) -> Result<String> {
    let previous_argument = previous_command.map_or_else(String::new, |previous| {
        format!(" --previous-command {}", shell_quote(previous))
    });
    let fallback = previous_command.map_or_else(
        || ":".to_owned(),
        |previous| format!("exec /bin/sh -c {}", shell_quote(previous)),
    );
    let metadata = serde_json::to_vec(&Bridge {
        program: program.to_owned(),
        previous_command: previous_command.map(str::to_owned),
    })?;
    let mut encoded = String::with_capacity(metadata.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in metadata {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 15) as usize] as char);
    }
    let program = shell_quote(program);
    Ok(format!(
        "if [ -n \"${{SHEPHRD_AGY_TTY-}}\" ] && [ -n \"${{SHEPHRD_AGY_PID-}}\" ] && [ -n \"${{SHEPHRD_AGY_START_TIME-}}\" ] && [ -x {program} ]; then exec {program} agy-status{previous_argument}; else {fallback}; fi{BRIDGE_MARKER}{BRIDGE_VERSION}{encoded}"
    ))
}

fn original_command(command: &str) -> Result<Option<String>> {
    let Some((_, encoded)) = command.rsplit_once(BRIDGE_MARKER) else {
        return Ok(Some(command.to_owned()));
    };
    let encoded = encoded
        .strip_prefix(BRIDGE_VERSION)
        .context("unsupported existing Shephrd Antigravity callback version")?;
    if encoded.len() % 2 != 0 {
        bail!("existing Shephrd Antigravity callback metadata is invalid");
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = nibble(pair[0]).context("invalid Antigravity callback metadata")?;
        let low = nibble(pair[1]).context("invalid Antigravity callback metadata")?;
        bytes.push((high << 4) | low);
    }
    let bridge: Bridge =
        serde_json::from_slice(&bytes).context("parse Antigravity callback metadata")?;
    if callback_command(&bridge.program, bridge.previous_command.as_deref())? != command {
        bail!("existing Shephrd Antigravity callback was edited; preserving it unchanged");
    }
    Ok(bridge.previous_command)
}

fn compose(settings: &mut Value, program: &str) -> Result<bool> {
    let root = settings
        .as_object_mut()
        .context("Antigravity settings must be an object")?;
    let original = root.get("statusLine");
    let mut status_line = original.cloned().unwrap_or_else(|| json!({}));
    let status = status_line
        .as_object_mut()
        .context("Antigravity statusLine must be an object")?;
    if let Some(kind) = status.get("type")
        && kind.as_str() != Some("command")
    {
        bail!("unsupported Antigravity statusLine type");
    }
    if let Some(enabled) = status.get("enabled") {
        match enabled.as_bool() {
            Some(true) => {}
            Some(false) => bail!("Antigravity statusLine is disabled; leaving it disabled"),
            None => bail!("Antigravity statusLine enabled must be a boolean"),
        }
    }
    if let Some(stack) = status.get("stack_with_default")
        && !stack.is_boolean()
    {
        bail!("Antigravity statusLine stack_with_default must be a boolean");
    }
    if let Some(padding) = status.get("padding")
        && padding.as_u64().is_none()
    {
        bail!("Antigravity statusLine padding must be a nonnegative integer");
    }
    let previous = match status.get("command") {
        Some(Value::String(command)) => original_command(command)?,
        Some(_) => bail!("Antigravity statusLine command must be a string"),
        None => None,
    };
    status.insert("type".to_owned(), json!("command"));
    status.insert(
        "command".to_owned(),
        json!(callback_command(program, previous.as_deref())?),
    );
    if previous.is_none() {
        // Without custom rendering, preserve the builtin and emit no extra text.
        status.insert("stack_with_default".to_owned(), json!(true));
    }
    if original == Some(&status_line) {
        return Ok(false);
    }
    root.insert("statusLine".to_owned(), status_line);
    Ok(true)
}

fn update_settings(home: &Path, program: &Path) -> Result<bool> {
    let program = program
        .to_str()
        .context("Antigravity callback program path is not UTF-8")?;
    let directory = settings_directory(home)?;
    let lock = open_child(
        &directory,
        LOCK_NAME,
        libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0o600,
    )
    .context("open Antigravity settings lock")?;
    validate_file(&lock.metadata()?, true).context("validate Antigravity settings lock")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("lock Antigravity settings");
    }

    let original = read_settings(&directory)?;
    let mut settings = match &original {
        Some(original) => serde_json::from_slice(&original.bytes)
            .context("Antigravity settings are not valid JSON; preserving them unchanged")?,
        None => json!({}),
    };
    if !compose(&mut settings, program)? {
        return Ok(false);
    }
    let mut bytes = serde_json::to_vec_pretty(&settings)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        bail!("Antigravity settings would exceed the safe size limit");
    }
    atomic_settings(&directory, original.as_ref(), &bytes)?;
    Ok(true)
}

fn settings_directory(home: &Path) -> Result<File> {
    let home = fs::canonicalize(home).context("locate Antigravity home directory")?;
    let home = CString::new(home.as_os_str().as_bytes())?;
    let descriptor = unsafe {
        libc::open(
            home.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).context("open Antigravity home directory");
    }
    let mut directory = unsafe { File::from_raw_fd(descriptor) };
    validate_directory(&directory.metadata()?)?;
    for name in [".gemini", "antigravity-cli"] {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        directory = match open_child(&directory, name, flags, 0) {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name_c = CString::new(name)?;
                let result =
                    unsafe { libc::mkdirat(directory.as_raw_fd(), name_c.as_ptr(), 0o700) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error).context("create private Antigravity settings directory");
                    }
                }
                open_child(&directory, name, flags, 0)
                    .context("open private Antigravity settings directory")?
            }
            Err(error) => return Err(error).context("open Antigravity settings directory"),
        };
        validate_directory(&directory.metadata()?)?;
    }
    Ok(directory)
}

fn validate_directory(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o200 == 0
    {
        bail!("Antigravity settings directory is not safely owned and writable");
    }
    Ok(())
}

fn validate_file(metadata: &fs::Metadata, private: bool) -> Result<()> {
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o200 == 0
        || (private && metadata.permissions().mode() & 0o077 != 0)
    {
        bail!("Antigravity settings file is not safely owned and writable");
    }
    Ok(())
}

fn open_child(directory: &File, name: &str, flags: i32, mode: u32) -> std::io::Result<File> {
    let name = CString::new(name).expect("static or internally generated filename");
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::mode_t,
        )
    };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[derive(PartialEq, Eq)]
struct SettingsSnapshot {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn read_settings(directory: &File) -> Result<Option<SettingsSnapshot>> {
    let file = match open_child(
        directory,
        SETTINGS_NAME,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context("open Antigravity settings without following links");
        }
    };
    let metadata = file.metadata()?;
    validate_file(&metadata, false).context("validate existing Antigravity settings")?;
    if metadata.len() > MAX_SETTINGS_BYTES {
        bail!("Antigravity settings exceed the safe size limit");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        bail!("Antigravity settings exceed the safe size limit");
    }
    Ok(Some(SettingsSnapshot {
        bytes,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }))
}

fn atomic_settings(
    directory: &File,
    original: Option<&SettingsSnapshot>,
    bytes: &[u8],
) -> Result<()> {
    let temporary = format!(
        ".settings.json.shephrd-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed),
    );
    let mut file = open_child(
        directory,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
    .context("create private Antigravity settings update")?;
    let temporary = CString::new(temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        if read_settings(directory)?.as_ref() != original {
            bail!("Antigravity settings changed during setup; preserving the newer settings");
        }
        let destination = CString::new(SETTINGS_NAME)?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("commit Antigravity settings update");
        }
        directory
            .sync_all()
            .context("sync Antigravity settings directory")
    })();
    if result.is_err() {
        unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::Arc;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
            fs::create_dir_all(&parent).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let directory = parent.join(format!(
                "agy-settings-test-{}-{}",
                std::process::id(),
                NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory)
        }

        fn settings(&self) -> PathBuf {
            self.0.join(".gemini/antigravity-cli/settings.json")
        }

        fn write_settings(&self, bytes: &[u8]) {
            settings_directory(&self.0).unwrap();
            fs::write(self.settings(), bytes).unwrap();
            fs::set_permissions(self.settings(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn builtin_configuration_preserves_settings_and_does_not_rewrite_on_relaunch() {
        let fixture = Fixture::new();
        fixture.write_settings(br#"{"model":"original","permissions":{"deny":["private"]}}"#);
        let program = Path::new("/missing/shephrd");
        assert!(update_settings(&fixture.0, program).unwrap());
        let first = fs::read(fixture.settings()).unwrap();
        let metadata = fs::metadata(fixture.settings()).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let settings: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(settings["model"], "original");
        assert_eq!(settings["permissions"], json!({"deny": ["private"]}));
        assert_eq!(settings["statusLine"]["stack_with_default"], true);
        assert!(!update_settings(&fixture.0, program).unwrap());
        assert_eq!(fs::read(fixture.settings()).unwrap(), first);
        assert_eq!(
            fs::metadata(fixture.settings()).unwrap().ino(),
            metadata.ino()
        );
    }

    #[test]
    fn custom_options_and_original_command_survive_an_upgrade_without_recursion() {
        let original = "printf 'user\\n'; cat; exit 37";
        let mut settings = json!({
            "statusLine": {"type": "command", "command": original, "enabled": true,
                "stack_with_default": false, "padding": 3, "future": {"keep": 1}},
            "unrelated": [1, 2]
        });
        let before = settings.clone();
        assert!(compose(&mut settings, "/old/shephrd").unwrap());
        assert!(compose(&mut settings, "/new/shephrd").unwrap());
        assert!(!compose(&mut settings, "/new/shephrd").unwrap());
        for key in ["enabled", "stack_with_default", "padding", "future"] {
            assert_eq!(settings["statusLine"][key], before["statusLine"][key]);
        }
        assert_eq!(settings["unrelated"], before["unrelated"]);
        let command = settings["statusLine"]["command"].as_str().unwrap();
        assert_eq!(
            original_command(command).unwrap().as_deref(),
            Some(original)
        );
        assert!(!command.contains("/old/shephrd"));
    }

    #[test]
    fn empty_existing_callback_keeps_explicit_display_options() {
        let mut settings = json!({
            "statusLine": {"type": "command", "command": "", "padding": 2,
                "stack_with_default": false}
        });
        assert!(compose(&mut settings, "/missing/shephrd").unwrap());
        assert_eq!(settings["statusLine"]["stack_with_default"], false);
        assert_eq!(settings["statusLine"]["padding"], 2);
        assert_eq!(
            original_command(settings["statusLine"]["command"].as_str().unwrap())
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn edited_or_future_bridge_is_not_recursively_wrapped() {
        let bridge = callback_command("/old/shephrd", Some("original")).unwrap();
        for command in [
            format!(":; {bridge}"),
            bridge.replace("shephrd-agy-status:v1:", "shephrd-agy-status:v2:"),
        ] {
            let mut settings = json!({"statusLine": {"command": command}});
            let before = settings.clone();
            assert!(compose(&mut settings, "/new/shephrd").is_err());
            assert_eq!(settings, before);
        }
    }

    #[test]
    fn disabled_malformed_and_unrecognized_settings_remain_byte_identical() {
        let fixture = Fixture::new();
        for bytes in [
            &b"{broken"[..],
            &b"[]"[..],
            &b"{\"statusLine\":false}"[..],
            &b"{\"statusLine\":{\"enabled\":false,\"command\":\"original\"}}"[..],
            &b"{\"statusLine\":{\"type\":\"future\",\"command\":\"original\"}}"[..],
            &b"{\"statusLine\":{\"command\":12}}"[..],
            &b"{\"statusLine\":{\"padding\":-1}}"[..],
        ] {
            fixture.write_settings(bytes);
            assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
            assert_eq!(fs::read(fixture.settings()).unwrap(), bytes);
        }
    }

    #[test]
    fn symlink_readonly_and_hardlinked_settings_are_not_replaced() {
        let fixture = Fixture::new();
        let target = fixture.0.join("external.json");
        fs::write(&target, b"{\"private\":true}").unwrap();
        settings_directory(&fixture.0).unwrap();
        symlink(&target, fixture.settings()).unwrap();
        assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
        assert!(
            fs::symlink_metadata(fixture.settings())
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"{\"private\":true}");
        fs::remove_file(fixture.settings()).unwrap();
        fs::hard_link(&target, fixture.settings()).unwrap();
        assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
        fs::remove_file(fixture.settings()).unwrap();
        fixture.write_settings(b"{}");
        fs::set_permissions(fixture.settings(), fs::Permissions::from_mode(0o400)).unwrap();
        assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
        assert_eq!(fs::read(fixture.settings()).unwrap(), b"{}");
        assert_eq!(
            fs::metadata(fixture.settings())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
    }

    #[test]
    fn settings_directory_symlinks_and_oversized_input_are_refused() {
        let fixture = Fixture::new();
        let target = fixture.0.join("external");
        fs::create_dir(&target).unwrap();
        symlink(&target, fixture.0.join(".gemini")).unwrap();
        assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
        assert!(!target.join("antigravity-cli").exists());
        fs::remove_file(fixture.0.join(".gemini")).unwrap();
        let bytes = vec![b' '; MAX_SETTINGS_BYTES as usize + 1];
        fixture.write_settings(&bytes);
        assert!(update_settings(&fixture.0, Path::new("/missing/shephrd")).is_err());
        assert_eq!(fs::read(fixture.settings()).unwrap(), bytes);
    }

    #[test]
    fn concurrent_first_launches_install_exactly_one_bridge() {
        let fixture = Arc::new(Fixture::new());
        fixture.write_settings(br#"{"statusLine":{"type":"command","command":"original"}}"#);
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let fixture = Arc::clone(&fixture);
                std::thread::spawn(move || {
                    update_settings(&fixture.0, Path::new("/missing/shephrd")).unwrap()
                })
            })
            .collect();
        let changes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|changed| *changed)
            .count();
        assert_eq!(changes, 1);
        let settings: Value =
            serde_json::from_slice(&fs::read(fixture.settings()).unwrap()).unwrap();
        assert_eq!(
            original_command(settings["statusLine"]["command"].as_str().unwrap())
                .unwrap()
                .as_deref(),
            Some("original")
        );
    }

    #[test]
    fn external_settings_change_aborts_atomic_replacement() {
        let fixture = Fixture::new();
        fixture.write_settings(b"{}");
        let directory = settings_directory(&fixture.0).unwrap();
        let original = read_settings(&directory).unwrap();
        fixture.write_settings(b"{\"changed_by_user\":true}");
        assert!(atomic_settings(&directory, original.as_ref(), b"{\"bridge\":true}").is_err());
        assert_eq!(
            fs::read(fixture.settings()).unwrap(),
            b"{\"changed_by_user\":true}"
        );
        assert_eq!(
            fs::read_dir(fixture.settings().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn relative_path_and_external_symlinks_cannot_resolve_back_to_either_wrapper() {
        let fixture = Fixture::new();
        for directory in ["wrappers", "aliases", "real"] {
            fs::create_dir(fixture.0.join(directory)).unwrap();
        }
        for name in ["agy", "antigravity"] {
            let path = fixture.0.join("wrappers").join(name);
            fs::write(&path, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        symlink(
            fixture.0.join("wrappers/antigravity"),
            fixture.0.join("aliases/agy"),
        )
        .unwrap();
        let executable = fixture.0.join("real/agy");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let resolved = resolve_executable(
            &fixture.0.join("wrappers"),
            "agy",
            OsStr::new("wrappers:aliases:real"),
            &fixture.0,
        )
        .unwrap();
        assert_eq!(resolved, executable);
    }

    #[test]
    fn only_known_interactive_invocations_are_configured() {
        let plan = |arguments: &[&str]| {
            interactive_arguments(
                &arguments
                    .iter()
                    .map(|argument| OsString::from(*argument))
                    .collect::<Vec<_>>(),
            )
        };
        assert!(plan(&[]));
        assert!(plan(&["--model=example", "--continue"]));
        assert!(plan(&["-i", "help with tests", "--sandbox"]));
        assert!(plan(&["--agent", "--print"]));
        assert!(plan(&["--", "--print"]));
        for arguments in [
            vec!["--print", "prompt"],
            vec!["--prompt=prompt"],
            vec!["-p", "prompt"],
            vec!["--help"],
            vec!["mcp", "list"],
            vec!["plugins", "list"],
            vec!["--model", "example", "update"],
            vec!["--future", "--print"],
            vec!["--new-project"],
            vec!["--agent"],
            vec!["--output-format=json"],
        ] {
            assert!(!plan(&arguments), "{arguments:?}");
        }
    }
}
