//! Native user-scoped Codex MCP registration; the managed proxy owns binding.
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use bkpr_protocol::HarnessKind;
use serde_json::json;

use super::types::{Asset, Bundle, Definition, Entry, Format, Paths, Removal, read_native};

fn native_home(paths: &Paths) -> Result<PathBuf> {
    let override_directory = paths
        .environment
        .get(std::ffi::OsStr::new("CODEX_HOME"))
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty());
    if override_directory.is_none() {
        return Ok(paths.home.join(".codex"));
    }
    let directory = paths.env_path("CODEX_HOME", paths.home.join(".codex"))?;
    ensure!(
        directory.is_dir(),
        "CODEX_HOME must name an existing directory"
    );
    fs::canonicalize(directory).context("resolve native CODEX_HOME")
}

/// Describe credential-free Codex MCP registration without native hook duplication.
pub(super) fn definition(paths: &Paths) -> Result<Definition> {
    let file = native_home(paths)?.join("config.toml");
    let manifest = paths.manifest(HarnessKind::Codex, &[&file]);
    let installation = manifest
        .to_str()
        .context("Codex installation record path is not UTF-8")?;
    let program = paths
        .program
        .to_str()
        .context("Codex MCP executable path is not UTF-8")?;
    let mut blockers = Vec::new();
    if let Some(bytes) = read_native(&file)? {
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            anyhow::anyhow!("invalid UTF-8 in Codex configuration {}", file.display())
        })?;
        let config: toml::Value = toml::from_str(text).map_err(|_| {
            anyhow::anyhow!("invalid Codex configuration TOML at {}", file.display())
        })?;
        if config
            .get("mcp_servers")
            .and_then(|servers| servers.get("bkpr"))
            .and_then(|server| server.get("enabled"))
            .and_then(toml::Value::as_bool)
            == Some(false)
        {
            blockers.push("Codex has explicitly disabled the Bkpr MCP server".into());
        }
    }
    let registration = json!({
        "command": program,
        "args": ["capture", "installed-mcp", "--harness", "codex", "--installation", installation],
        "env_vars": ["BKPR_AGENT_RELAY_SOCKET", "BKPR_AGENT_RELAY_TOKEN"]
    });
    let entries = ["command", "args", "env_vars"]
        .into_iter()
        .map(|key| {
            Entry::value(
                key,
                file.clone(),
                Format::Toml,
                &["mcp_servers", "bkpr", key],
                registration[key].clone(),
            )
        })
        .collect();
    let bundle = Bundle::new(
        paths,
        HarnessKind::Codex,
        vec![Asset::text(
            "registration.json",
            serde_json::to_string(&registration)?,
        )],
    )?;
    Ok(Definition {
        kind: HarnessKind::Codex,
        bundle,
        entries,
        files: Vec::new(),
        removals: vec![Removal {
            file,
            format: Format::Toml,
            keys: vec!["mcp_servers".into(), "bkpr".into()],
            allowed: [
                "tools",
                "default_tools_approval_mode",
                "enabled",
                "enabled_tools",
                "disabled_tools",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }],
        blockers,
        notes: vec![
            "Start a new managed Codex session to load the configured MCP; the private app-server/proxy still owns exact conversation binding.".into(),
            "Native policies, disabled state and tool approvals are unchanged. The installed entry stores relay environment names only; relay values exist only during managed launches.".into(),
            "Explicit uninstall removes the logical Bkpr MCP entry and its Bkpr-specific native tool preferences; other servers and permission settings are untouched.".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::symlink;

    #[test]
    fn redirected_native_home_is_canonical_and_never_created_by_inspection() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut paths = Paths {
            home: root.path().join("home"),
            data: root.path().join("data"),
            program: root.path().join("bkpr"),
            cwd: root.path().to_owned(),
            environment: BTreeMap::from([("CODEX_HOME".into(), "missing".into())]),
        };
        assert!(native_home(&paths).is_err());
        assert!(!root.path().join("missing").exists());
        let directory = root.path().join("native account");
        fs::create_dir(&directory)?;
        symlink(&directory, root.path().join("redirect"))?;
        paths
            .environment
            .insert("CODEX_HOME".into(), "redirect".into());
        assert_eq!(native_home(&paths)?, directory.canonicalize()?);
        Ok(())
    }
}
