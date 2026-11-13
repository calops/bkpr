//! Native Antigravity MCP registration, preserving explicit disabled state.
use super::types::{Asset, Bundle, Definition, Entry, Format, Paths, Removal, read_native};
use anyhow::{Context, Result};
use bkpr_protocol::HarnessKind;
use serde_json::{Value, json};

/// Describe the current account's credential-free native Antigravity registration.
pub(super) fn definition(paths: &Paths) -> Result<Definition> {
    let file = paths.home.join(".gemini/config/mcp_config.json");
    let manifest = paths.manifest(HarnessKind::Agy, &[&file]);
    let args = json!([
        "capture",
        "installed-mcp",
        "--harness",
        "agy",
        "--installation",
        manifest
    ]);
    let mut blockers = Vec::new();
    if let Some(bytes) = read_native(&file)? {
        let config: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid native Antigravity JSON configuration"))?;
        let object = config
            .as_object()
            .context("Antigravity MCP configuration must be an object")?;
        if let Some(disabled) = object.get("disabledServers") {
            let names = disabled
                .as_array()
                .context("Antigravity disabledServers must be an array")?;
            if names.iter().any(|name| name.as_str() == Some("bkpr")) {
                blockers.push("Antigravity has disabled the Bkpr MCP server".into());
            }
        }
        if config
            .get("mcpServers")
            .and_then(|servers| servers.get("bkpr"))
            .and_then(|server| server.get("disabled"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            blockers.push("Antigravity has explicitly disabled the Bkpr MCP server".into());
        }
    }
    let command = serde_json::to_value(&paths.program)?;
    let registration = json!({"command":command,"args":args});
    Ok(Definition {
        kind: HarnessKind::Agy,
        bundle: Bundle::new(paths, HarnessKind::Agy, vec![Asset::text("registration.json", serde_json::to_string(&registration)?)])?,
        entries: vec![
            Entry::value("command", file.clone(), Format::Json, &["mcpServers", "bkpr", "command"], command),
            Entry::value("args", file.clone(), Format::Json, &["mcpServers", "bkpr", "args"], args),
        ],
        files: Vec::new(),
        removals: vec![Removal { file, format: Format::Json, keys: vec!["mcpServers".into(),"bkpr".into()], allowed: vec!["disabled".into()] }],
        blockers,
        notes: vec!["Start a new managed Antigravity conversation to load the native MCP. Root-conversation metadata still owns binding; this command does not grant native tool approvals.".into()],
    })
}
