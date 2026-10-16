//! Launch-local OMP extension and MCP registration; never writes OMP settings.
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tempfile::TempDir;

use crate::relay::{self, RelayLaunch};

const EXTENSION: &str = include_str!("../../../integration/omp/index.ts");

pub fn wrapper(program: &Path, directory: &Path) -> Result<String> {
    let program = program
        .to_str()
        .context("OMP wrapper program path is not UTF-8")?;
    let directory = directory
        .to_str()
        .context("OMP wrapper directory is not UTF-8")?;
    Ok(format!(
        "#!/bin/sh\nexec {} omp-launch --wrapper-dir {} -- \"$@\"\n",
        crate::claude::shell_quote(program),
        crate::claude::shell_quote(directory),
    ))
}

fn real_omp(directory: &Path) -> Result<PathBuf> {
    let wrapper = fs::canonicalize(directory.join("omp"))?;
    for entry in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let candidate = entry.join("omp");
        if fs::metadata(&candidate)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            && let Ok(resolved) = fs::canonicalize(&candidate)
            && resolved != wrapper
        {
            // Bare relative paths would cause Command to search PATH again.
            return if candidate.is_absolute() {
                Ok(candidate)
            } else {
                Ok(env::current_dir()?.join(candidate))
            };
        }
    }
    bail!("OMP is not installed on PATH outside the session wrapper")
}

fn check_arguments(arguments: &[OsString]) -> Result<()> {
    for argument in arguments {
        if argument == "--" {
            break;
        }
        if let Some(argument) = argument.to_str()
            && (argument == "--trusted-extension" || argument.starts_with("--trusted-extension="))
        {
            bail!(
                "Bkpr OMP communication requires a launch-local extension package (-e), \
                 which OMP cannot combine with --trusted-extension"
            );
        }
    }
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create OMP launch file {}", path.display()))?
        .write_all(contents)
        .with_context(|| format!("write OMP launch file {}", path.display()))
}

fn extension_package(
    runtime: &Path,
    program: &Path,
    cwd: &Path,
    relay: &RelayLaunch,
) -> Result<TempDir> {
    // Keep credentials in the session's private runtime, never in OMP config.
    let directory = tempfile::Builder::new()
        .prefix("omp-")
        .tempdir_in(runtime)
        .context("create launch-local OMP extension package")?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let server_name = format!("bkpr-{}", relay.agent_id);
    let manifest = json!({
        "name": server_name,
        "private": true,
        "type": "module",
        "omp": {"extensions": ["index.ts"]},
    });
    let config = json!({
        "mcpServers": {
            (server_name): {
                "type": "stdio",
                "command": program,
                "args": ["relay-mcp"],
                "cwd": cwd,
                "env": {
                    "BKPR_RELAY_AGENT_ID": relay.agent_id,
                    "BKPR_RELAY_TOKEN": relay.token,
                    "BKPR_RELAY_SOCKET": relay.socket,
                    "BKPR_RELAY_HARNESS": relay.harness,
                },
            },
        },
    });
    write_private(
        &directory.path().join("package.json"),
        &serde_json::to_vec(&manifest)?,
    )?;
    write_private(&directory.path().join("index.ts"), EXTENSION.as_bytes())?;
    write_private(
        &directory.path().join("mcp.json"),
        &serde_json::to_vec(&config)?,
    )?;
    Ok(directory)
}

pub fn launch(directory: &Path, arguments: Vec<OsString>) -> Result<()> {
    check_arguments(&arguments)?;
    let executable = real_omp(directory)?;
    let cwd = env::current_dir().context("locate OMP working directory")?;
    let program = env::current_exe().context("locate Bkpr MCP executable")?;
    let relay = relay::prepare("omp", &cwd)?;
    let runtime = directory.parent().context("locate session runtime")?;
    let package = extension_package(runtime, &program, &cwd, &relay)?;
    let mut command = Command::new(executable);
    relay.apply(&mut command);
    command
        .env("BKPR_OMP_SERVER", format!("bkpr-{}", relay.agent_id))
        .arg("-e")
        .arg(package.path())
        .args(arguments);
    // Both processes share the foreground terminal group. OMP owns Ctrl-C;
    // the waiting wrapper must not exit while OMP handles an interrupted turn.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    // Keep the registration's process alive while OMP runs, and remove its
    // credential-bearing package on normal exit instead of leaking temp files.
    let status = command.status().context("execute OMP");
    drop(package);
    let status = status?;
    if !status.success() {
        std::process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_extension_conflict_does_not_consume_prompt() {
        assert!(check_arguments(&["--trusted-extension=/tmp/ext.ts".into()]).is_err());
        assert!(check_arguments(&["--trusted-extension".into(), "/tmp/ext.ts".into()]).is_err());
        assert!(check_arguments(&["--".into(), "--trusted-extension".into()]).is_ok());
        assert!(check_arguments(&["--no-extensions".into(), "a prompt".into()]).is_ok());
    }

    #[test]
    fn temporary_credentials_are_private_and_removed_after_launch() {
        let runtime = tempfile::tempdir().unwrap();
        let relay = RelayLaunch {
            agent_id: "agent-test".into(),
            token: "secret-test".into(),
            socket: PathBuf::from("/run/bkpr-relay/socket"),
            harness: "omp".into(),
        };
        let package = extension_package(
            runtime.path(),
            Path::new("/opt/bkpr"),
            Path::new("/work"),
            &relay,
        )
        .unwrap();
        let package_path = package.path().to_path_buf();
        assert_eq!(
            fs::metadata(&package_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["index.ts", "mcp.json", "package.json"] {
            assert_eq!(
                fs::metadata(package_path.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(package);
        assert!(!package_path.exists());
    }
}
