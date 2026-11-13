//! OpenCode 2 native server/CLI plugins and dormant shared MCP configuration.
use anyhow::{Context, Result};
use bkpr_protocol::HarnessKind;
use serde_json::json;

use super::types::{Asset, Bundle, Definition, Entry, Format, Paths, Removal, read_native};

fn native_blockers(directory: &std::path::Path, marker: &str) -> Result<Vec<String>> {
    let mut blockers = Vec::new();
    let mut mcp_disabled = false;
    let mut server_disabled = false;
    let mut cli_disabled = false;
    for name in ["opencode.json", "opencode.jsonc", "cli.json"] {
        let path = directory.join(name);
        let Some(bytes) = read_native(&path)? else {
            continue;
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            anyhow::anyhow!(
                "native OpenCode preferences must be UTF-8 at {}",
                path.display()
            )
        })?;
        let parsed = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
            .map_err(|_| {
                anyhow::anyhow!("invalid native OpenCode preferences at {}", path.display())
            })?
            .value;
        let parsed: serde_json::Value = parsed.context("empty native OpenCode preferences")?.into();
        if name != "cli.json"
            && let Some(value) = parsed
                .pointer("/mcp/servers/bkpr/disabled")
                .and_then(serde_json::Value::as_bool)
        {
            mcp_disabled = value;
        }
        let disabled = if name == "cli.json" {
            &mut cli_disabled
        } else {
            &mut server_disabled
        };
        for plugin in parsed
            .get("plugins")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(target) = plugin
                .as_str()
                .or_else(|| plugin.get("package").and_then(serde_json::Value::as_str))
            else {
                continue;
            };
            if let Some(target) = target.strip_prefix('-') {
                if matches!(target, "*" | "bkpr-native" | "bkpr-opencode-native")
                    || target.contains(marker)
                {
                    *disabled = true;
                }
            } else if target.contains(marker) {
                *disabled = false;
            }
        }
    }
    if mcp_disabled {
        blockers.push("OpenCode's effective global configuration disables the native bkpr MCP server; enable it natively before installing or launching managed capture.".into());
    }
    if server_disabled || cli_disabled {
        blockers.push("OpenCode's native server or CLI configuration disables the Bkpr plugin; change that directive natively before installing or launching managed capture.".into());
    }
    Ok(blockers)
}

/// Plan the same immutable native package in OpenCode's server and CLI config.
pub(super) fn definition(paths: &Paths) -> Result<Definition> {
    // OpenCode 2.2.7 util/global.ts overrides the XDG global root with
    // OPENCODE_CONFIG_DIR for both core config discovery and CLI config.
    let xdg = if paths
        .environment
        .get(std::ffi::OsStr::new("XDG_CONFIG_HOME"))
        .is_none_or(|value| value.is_empty())
    {
        paths.home.join(".config")
    } else {
        paths.env_path("XDG_CONFIG_HOME", paths.home.join(".config"))?
    };
    let directory = if paths
        .environment
        .get(std::ffi::OsStr::new("OPENCODE_CONFIG_DIR"))
        .is_some_and(|value| value.is_empty())
    {
        paths.cwd.clone()
    } else {
        paths.env_path("OPENCODE_CONFIG_DIR", xdg.join("opencode"))?
    };
    let server = if directory.join("opencode.jsonc").try_exists()? {
        directory.join("opencode.jsonc")
    } else if directory.join("opencode.json").try_exists()? {
        directory.join("opencode.json")
    } else {
        directory.join("opencode.jsonc")
    };
    let cli = directory.join("cli.json");
    let mut blockers = Vec::new();
    if !cli.try_exists()? && directory.join("tui.json").try_exists()? {
        blockers.push("OpenCode must migrate its existing tui.json before installation: start native OpenCode 2 once so it creates cli.json without losing native preferences.".into());
    }
    let bundle = Bundle::new(
        paths,
        HarnessKind::Opencode,
        vec![
            Asset::borrowed(
                "package.json",
                br#"{"name":"bkpr-opencode-native","private":true,"type":"module"}"#,
            ),
            Asset::borrowed(
                "opencode/index.ts",
                include_bytes!("../../../../../integration/opencode/index.ts"),
            ),
            Asset::borrowed(
                "opencode/tui.ts",
                include_bytes!("../../../../../integration/opencode/tui.ts"),
            ),
            Asset::borrowed(
                "opencode/rpc.ts",
                include_bytes!("../../../../../integration/opencode/rpc.ts"),
            ),
            Asset::borrowed(
                "opencode/session.ts",
                include_bytes!("../../../../../integration/opencode/session.ts"),
            ),
            Asset::borrowed(
                "shared/native.ts",
                include_bytes!("../../../../../integration/shared/native.ts"),
            ),
        ],
    )?;
    let package = url::Url::from_directory_path(bundle.root.join("opencode"))
        .map_err(|()| anyhow::anyhow!("OpenCode native package path must be absolute"))?
        .to_string();
    let plugin = json!({ "package": package });
    let marker = "/bkpr/integrations/opencode/".to_owned();
    blockers.extend(native_blockers(&directory, &marker)?);
    let manifest = paths.manifest(HarnessKind::Opencode, &[server.as_path(), cli.as_path()]);
    let mut entries = vec![
        Entry::array(
            "server-plugin",
            server.clone(),
            Format::Json,
            &["plugins"],
            plugin.clone(),
            marker.clone(),
        ),
        Entry::array(
            "cli-plugin",
            cli,
            Format::Json,
            &["plugins"],
            plugin,
            marker,
        ),
    ];
    let mcp = json!({
        "type": "local", "codemode": false,
        "command": [paths.program, "capture", "installed-mcp", "--harness", "opencode", "--installation", manifest]
    });
    for key in ["type", "codemode", "command"] {
        entries.push(Entry::value(
            &format!("mcp-{key}"),
            server.clone(),
            Format::Json,
            &["mcp", "servers", "bkpr", key],
            mcp[key].clone(),
        ));
    }
    Ok(Definition { kind: HarnessKind::Opencode, bundle, entries, files: vec![],
        removals: vec![Removal {file:server,format:Format::Json,keys:vec!["mcp".into(),"servers".into(),"bkpr".into()],allowed:vec!["disabled".into()]}],
        blockers, notes: vec![
            "OpenCode 2.2.7 native server and CLI plugins; restart OpenCode to load. Native config remains authoritative for plugin enablement, MCP and approvals. Managed interactive launches retain --standalone; captureFile and recovery options are launch-private and never installed.".into(),
        ] })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &std::path::Path) -> Paths {
        Paths {
            home: root.join("home"),
            data: root.join("data"),
            program: root.join("bin/bkpr"),
            cwd: root.join("project"),
            environment: Default::default(),
        }
    }

    #[test]
    fn installing_does_not_suppress_native_legacy_preferences_migration() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let directory = paths.home.join(".config/opencode");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("tui.json"),
            r#"{"theme":"native","plugin":["native"]}"#,
        )
        .unwrap();
        assert!(
            definition(&paths)
                .unwrap()
                .blockers
                .iter()
                .any(|reason| reason.contains("migrate"))
        );
        assert!(!directory.join("cli.json").exists());
        std::fs::write(directory.join("cli.json"), "{}").unwrap();
        assert!(definition(&paths).unwrap().blockers.is_empty());
    }

    #[test]
    fn native_plugin_and_mcp_opt_outs_are_blockers_not_implicit_approvals() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let directory = paths.home.join(".config/opencode");
        std::fs::create_dir_all(&directory).unwrap();
        let cli = directory.join("cli.json");
        std::fs::write(&cli, "{/* native opt-out */ \"plugins\":[\"-*\"],}").unwrap();
        assert!(!definition(&paths).unwrap().blockers.is_empty());
        std::fs::write(&cli, "{}").unwrap();
        std::fs::write(
            directory.join("opencode.jsonc"),
            r#"{"mcp":{"servers":{"bkpr":{"type":"local","command":["native"],"disabled":true}}}}"#,
        )
        .unwrap();
        assert!(!definition(&paths).unwrap().blockers.is_empty());
    }
}
