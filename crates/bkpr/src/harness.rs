//! Explicit supported-harness dispatch and recursion-safe executable lookup.
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Harness {
    #[value(alias = "claude-code")]
    Claude,
    Codex,
    Omp,
    Opencode,
    #[value(name = "agy", alias = "antigravity")]
    Antigravity,
    #[value(name = "cursor", alias = "cursor-agent")]
    Cursor,
    #[value(alias = "copilot-cli", alias = "github-copilot")]
    Copilot,
    Aider,
    #[value(alias = ".cline")]
    Cline,
    Goose,
}

impl Harness {
    pub(crate) fn launch(self, arguments: Vec<OsString>) -> Result<()> {
        let names: &[&str] = match self {
            Self::Claude => return crate::claude::launch(None, arguments),
            Self::Codex => return crate::codex::launch(None, arguments),
            Self::Omp => return crate::omp::launch(None, arguments),
            Self::Opencode => &["opencode"],
            Self::Antigravity => return crate::agy::launch(None, arguments),
            Self::Cursor => &["cursor-agent"],
            Self::Copilot => &["copilot"],
            Self::Aider => &["aider"],
            Self::Cline => &["cline", ".cline"],
            Self::Goose => &["goose"],
        };
        let mut error = None;
        for name in names {
            match resolve(name, None) {
                Ok(executable) => {
                    // Recognition alone must not inherit another launch's messaging identity.
                    return Err(Command::new(executable)
                        .env_remove("BKPR_RELAY_AGENT_ID")
                        .env_remove("BKPR_RELAY_TOKEN")
                        .env_remove("BKPR_RELAY_SOCKET")
                        .env_remove("BKPR_RELAY_HARNESS")
                        .args(arguments)
                        .exec())
                    .with_context(|| format!("execute {name}"));
                }
                Err(cause) => error = Some(cause),
            }
        }
        Err(error.expect("each harness has an executable"))
    }
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn managed_script(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    let mut buffer = [0; 8192];
    let bytes = &mut buffer[..metadata.len().min(8192) as usize];
    if file.read_exact(bytes).is_err() || !bytes.starts_with(b"#!") {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.contains("--wrapper-dir")
        && ["claude-launch", "codex-launch", "omp-launch", "agy-launch"]
            .iter()
            .any(|name| text.contains(name))
}

pub(crate) fn resolve(name: &str, wrapper_directory: Option<&Path>) -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let inherited = env::var_os("BKPR_HARNESS_WRAPPER_DIR").map(PathBuf::from);
    let directories = wrapper_directory.into_iter().chain(inherited.as_deref());
    let mut excluded = Vec::new();
    for directory in directories {
        for managed in ["claude", "codex", "omp", "agy", "antigravity"] {
            if let Ok(metadata) = fs::metadata(directory.join(managed)) {
                excluded.push(metadata);
            }
        }
    }
    if let Ok(executable) = env::current_exe()
        && let Ok(metadata) = fs::metadata(executable)
    {
        excluded.push(metadata);
    }
    resolve_on_path(name, &path, &env::current_dir()?, &excluded)
}

fn resolve_on_path(
    name: &str,
    path: &OsStr,
    cwd: &Path,
    excluded: &[fs::Metadata],
) -> Result<PathBuf> {
    for directory in env::split_paths(path) {
        let candidate = if directory.is_absolute() {
            directory.join(name)
        } else {
            cwd.join(directory).join(name)
        };
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || excluded.iter().any(|other| same_file(&metadata, other))
            || managed_script(&candidate)
        {
            continue;
        }
        // Preserve the selected spelling for executable symlinks (including Nix wrappers).
        return Ok(candidate);
    }
    bail!("{name} is not installed on PATH outside Bkpr-managed wrappers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn resolves_executable_after_wrappers_and_non_executables() {
        let root = tempfile::tempdir().unwrap();
        for name in ["wrapper", "invalid", "real"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        executable(
            &root.path().join("wrapper/claude"),
            "#!/bin/sh\nexec bkpr claude-launch --wrapper-dir /gone -- \"$@\"\n",
        );
        fs::write(root.path().join("invalid/claude"), "not executable").unwrap();
        executable(&root.path().join("real/claude"), "#!/bin/sh\nexit 0\n");
        let path = env::join_paths(["wrapper", "invalid", "real"]).unwrap();
        assert_eq!(
            resolve_on_path("claude", &path, root.path(), &[]).unwrap(),
            root.path().join("real/claude")
        );
    }

    #[test]
    fn skips_symlink_and_hardlink_wrapper_identities() {
        let root = tempfile::tempdir().unwrap();
        executable(&root.path().join("managed"), "#!/bin/sh\nexit 0\n");
        fs::create_dir(root.path().join("alias")).unwrap();
        fs::create_dir(root.path().join("hard")).unwrap();
        symlink("../managed", root.path().join("alias/codex")).unwrap();
        fs::hard_link(root.path().join("managed"), root.path().join("hard/codex")).unwrap();
        let path = env::join_paths(["alias", "hard"]).unwrap();
        let excluded = [fs::metadata(root.path().join("managed")).unwrap()];
        assert!(resolve_on_path("codex", &path, root.path(), &excluded).is_err());
    }

    #[test]
    fn empty_path_component_returns_absolute_path_and_preserves_symlink() {
        let root = tempfile::tempdir().unwrap();
        executable(&root.path().join("actual"), "#!/bin/sh\nexit 0\n");
        symlink("actual", root.path().join("omp")).unwrap();
        assert_eq!(
            resolve_on_path("omp", OsStr::new(""), root.path(), &[]).unwrap(),
            root.path().join("omp")
        );
    }
}
