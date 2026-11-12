//! Private, launch-lifetime assets and shared MCP credential bootstrap.
use super::{Capture, validate_text};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{CaptureGrant, HarnessKind};
use serde::{Deserialize, Serialize};
use std::{
    fs::{OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials<Grant = CaptureGrant, DataPath = PathBuf> {
    grant: Grant,
    program: DataPath,
    socket: DataPath,
}

/// Create an owner-only temporary directory for launch-local assets.
pub(super) fn private_directory(prefix: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(Permissions::from_mode(0o700))
        .tempdir()
        .context("create private launch-local harness directory")
}

/// Create a new launch-local asset readable and writable only by its owner.
pub(super) fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("create launch-local harness asset")?
        .write_all(contents)
        .context("write launch-local harness asset")
}

/// Store the launch grant and callback paths in a private credential file.
pub(super) fn write_credentials(capture: &Capture, path: &Path) -> Result<()> {
    let credentials = Credentials {
        grant: &capture.grant,
        program: capture.program.as_path(),
        socket: capture.socket.as_path(),
    };
    write_private(path, &serde_json::to_vec(&credentials)?)
}

fn read_credentials(path: &Path, kind: HarnessKind) -> Result<Capture> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .context("open launch-local capture credentials")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o777 == 0o600
            && metadata.nlink() == 1
            && metadata.len() <= MAX_CREDENTIAL_BYTES,
        "capture credentials must be a bounded private owned regular file (0600)"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CREDENTIAL_BYTES,
        "capture credentials exceed limit"
    );
    let credentials: Credentials = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid launch-local capture credentials"))?;
    ensure!(
        credentials.grant.kind == kind && kind.is_managed(),
        "capture credential harness mismatch"
    );
    validate_text(&credentials.grant.launch_id, 512, "capture launch ID")?;
    validate_text(&credentials.grant.token, 512, "capture capability")?;
    validate_text(&credentials.grant.pane_id, 512, "capture pane ID")?;
    ensure!(
        credentials.program.is_absolute() && credentials.socket.is_absolute(),
        "capture paths must be absolute"
    );
    Ok(Capture {
        grant: credentials.grant,
        program: credentials.program,
        socket: credentials.socket,
        revoke_on_drop: false,
        ended: AtomicBool::new(false),
    })
}

/// Replace this process with the shared MCP using private launch credentials.
pub(super) fn exec_mcp(path: &Path, kind: HarnessKind) -> Result<i32> {
    let capture = read_credentials(path, kind)?;
    if kind == HarnessKind::Agy {
        bail!("Antigravity uses its native installed MCP authentication");
    }
    let mut command =
        Command::new(std::env::current_exe().context("locate shared MCP executable")?);
    capture.apply(&mut command);
    command.args(["capture", "mcp", "--harness", kind.as_str()]);
    Err(command.exec()).context("execute launch-authenticated shared MCP")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn credential_files_reject_exposed_permissions_and_aliases() {
        let directory = private_directory("bkpr-private-assets-").unwrap();
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let path = directory.path().join("credentials");
        let contents = serde_json::json!({
            "grant": {
                "launch_id": "fixture-launch",
                "token": "t".repeat(64),
                "pane_id": "fixture-pane",
                "kind": "opencode"
            },
            "program": "/fixture/bkpr",
            "socket": "/fixture/socket"
        });
        write_private(&path, &serde_json::to_vec(&contents).unwrap()).unwrap();
        assert!(read_credentials(&path, HarnessKind::Opencode).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_credentials(&path, HarnessKind::Opencode).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = directory.path().join("alias");
        fs::hard_link(&path, &alias).unwrap();
        assert!(read_credentials(&path, HarnessKind::Opencode).is_err());
        fs::remove_file(alias).unwrap();
        let link = directory.path().join("symlink");
        symlink(&path, &link).unwrap();
        assert!(read_credentials(&link, HarnessKind::Opencode).is_err());
    }

    #[test]
    fn credential_decode_errors_never_include_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials");
        write_private(&path, br#"{"grant":{"kind":"private-secret-value"}}"#).unwrap();
        let error = read_credentials(&path, HarnessKind::Opencode)
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains("private-secret-value"));
    }
}
