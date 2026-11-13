//! Omp's profile-scoped native discovery loader and shared MCP configuration.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use bkpr_protocol::HarnessKind;
use serde_json::json;

use super::types::{
    Asset, Bundle, Definition, Entry, Format, OwnedFile, Paths, Removal, read_native,
};
use saphyr::{LoadableYamlNode, Yaml};

fn profile(value: Option<&std::ffi::OsString>) -> Result<Option<&str>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.to_str().context("Omp profile must be UTF-8")?.trim();
    if value.is_empty() || value == "default" {
        return Ok(None);
    }
    let base = value.split('.').next().unwrap_or_default();
    let reserved = matches!(base, "con" | "prn" | "aux" | "nul")
        || (base.len() == 4
            && (base.starts_with("com") || base.starts_with("lpt"))
            && base.as_bytes()[3].is_ascii_digit());
    ensure!(
        value.len() <= 64
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || b"._-".contains(&byte))
            && !value.ends_with('.')
            && !reserved,
        "invalid native Omp profile"
    );
    Ok(Some(value))
}

fn agent_directory(paths: &Paths) -> Result<PathBuf> {
    // Omp 18.19: pi-utils/dirs.ts selects OMP_PROFILE before PI_PROFILE;
    // getAgentDir() is the unflattened native config root, not an XDG data path.
    let selected = profile(
        paths
            .environment
            .get(std::ffi::OsStr::new("OMP_PROFILE"))
            .or_else(|| paths.environment.get(std::ffi::OsStr::new("PI_PROFILE"))),
    )?;
    let config = paths
        .environment
        .get(std::ffi::OsStr::new("PI_CONFIG_DIR"))
        .filter(|value| !value.is_empty())
        .map(|value| value.to_str().context("Omp config directory must be UTF-8"))
        .transpose()?
        .unwrap_or(".omp");
    // Native path.join(home, config) retains home even for a leading slash.
    let root = paths.home.join(config.trim_start_matches('/'));
    if let Some(selected) = selected {
        return Ok(root.join("profiles").join(selected).join("agent"));
    }
    let fallback = root.join("agent");
    let legacy_profile = profile(paths.environment.get(std::ffi::OsStr::new("PI_PROFILE")))
        .ok()
        .flatten();
    let inherited = legacy_profile.map(|name| root.join("profiles").join(name).join("agent"));
    if paths
        .environment
        .get(std::ffi::OsStr::new("PI_CODING_AGENT_DIR"))
        .is_some_and(|value| {
            inherited
                .as_ref()
                .is_some_and(|path| path.as_os_str() == value)
        })
    {
        return Ok(fallback);
    }
    if paths
        .environment
        .get(std::ffi::OsStr::new("PI_CODING_AGENT_DIR"))
        .is_none_or(|value| value.is_empty())
    {
        return Ok(fallback);
    }
    paths.env_path("PI_CODING_AGENT_DIR", fallback)
}

fn disabled_preference(path: &std::path::Path) -> Result<Option<bool>> {
    let Some(bytes) = read_native(path)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        anyhow::anyhow!("native Omp preferences must be UTF-8 at {}", path.display())
    })?;
    let disabled: Option<Vec<String>> = if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let parsed = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
            .map_err(|_| anyhow::anyhow!("invalid native Omp preferences at {}", path.display()))?
            .value;
        let parsed: serde_json::Value = parsed.context("empty native Omp preferences")?.into();
        parsed
            .get("disabledExtensions")
            .map(|value| {
                serde_json::from_value(value.clone()).map_err(|_| {
                    anyhow::anyhow!("native Omp disabledExtensions must contain strings")
                })
            })
            .transpose()?
    } else {
        let documents = Yaml::load_from_str(text)
            .map_err(|_| anyhow::anyhow!("invalid native Omp preferences at {}", path.display()))?;
        ensure!(
            documents.len() <= 1,
            "native Omp preferences must contain one YAML document"
        );
        documents
            .first()
            .and_then(|document| document.as_mapping_get("disabledExtensions"))
            .map(|value| -> Result<Vec<String>> {
                value
                    .as_sequence()
                    .context("native Omp disabledExtensions must be an array")?
                    .iter()
                    .map(|entry| {
                        entry
                            .as_str()
                            .map(str::to_owned)
                            .context("native Omp disabledExtensions must contain strings")
                    })
                    .collect()
            })
            .transpose()?
    };
    Ok(disabled.map(|values| values.iter().any(|value| value == "extension-module:bkpr")))
}

fn native_blockers(
    paths: &Paths,
    agent: &std::path::Path,
    overlays: &[PathBuf],
) -> Result<Vec<String>> {
    let mut sources = Vec::new();
    for names in [
        vec![
            agent.join("config.yml"),
            agent.join("config.yaml"),
            agent.join("settings.json"),
        ],
        vec![
            paths.cwd.join(".omp/config.yml"),
            paths.cwd.join(".omp/config.yaml"),
            paths.cwd.join(".omp/settings.json"),
        ],
    ] {
        for path in names {
            if path.try_exists()? {
                sources.push(path);
                break;
            }
        }
    }
    if let Some(files) = paths
        .environment
        .get(std::ffi::OsStr::new("PI_CONFIG_FILES"))
    {
        for path in std::env::split_paths(files).filter(|path| !path.as_os_str().is_empty()) {
            let path = if let Ok(value) = path.strip_prefix("~/") {
                paths.home.join(value)
            } else {
                path
            };
            sources.push(if path.is_absolute() {
                path
            } else {
                paths.cwd.join(path)
            });
        }
    }
    for path in overlays {
        let path = if let Ok(value) = path.strip_prefix("~/") {
            paths.home.join(value)
        } else {
            path.clone()
        };
        sources.push(if path.is_absolute() {
            path
        } else {
            paths.cwd.join(path)
        });
    }
    let mut disabled = false;
    let mut blockers = Vec::new();
    for source in sources {
        match disabled_preference(&source) {
            Ok(Some(value)) => disabled = value,
            Ok(None) => {}
            Err(_) => blockers.push(format!("Cannot inspect native Omp extension preferences at {}; correct the native configuration before installing or launching managed capture.", source.display())),
        }
    }
    if disabled {
        blockers.push("Omp's effective native disabledExtensions disables extension-module:bkpr; enable it natively before installing or launching managed capture.".into());
    }
    Ok(blockers)
}

/// Plan Omp's native extension discovery and dormant launch-authenticated MCP.
pub(super) fn definition(paths: &Paths, overlays: &[PathBuf]) -> Result<Definition> {
    let agent = agent_directory(paths)?;
    let mut blockers = native_blockers(paths, &agent, overlays)?;
    let bundle = Bundle::new(paths, HarnessKind::Omp, vec![
        Asset::borrowed("omp/index.ts", include_bytes!("../../../../../integration/omp/index.ts")),
        Asset::borrowed("shared/native.ts", include_bytes!("../../../../../integration/shared/native.ts")),
        Asset::borrowed("package.json", br#"{"name":"bkpr-omp-native","private":true,"type":"module","omp":{"extensions":["omp/index.ts"]}}"#),
    ])?;
    let loader_path = agent.join("extensions/bkpr.ts");
    let source = pathdiff::diff_paths(
        bundle.root.join("omp/index.ts"),
        loader_path
            .parent()
            .context("Omp extension has no directory")?,
    )
    .context("Omp native extension must be reachable by relative import")?;
    let source = format!(
        "./{}",
        source
            .to_str()
            .context("Omp native extension path must be UTF-8")?
    );
    let loader = OwnedFile::text(
        "extension",
        loader_path,
        format!(
            "export {{ default }} from {};\n",
            serde_json::to_string(&source)?
        ),
    );
    let mcp_file =
        if !agent.join("mcp.json").try_exists()? && agent.join(".mcp.json").try_exists()? {
            agent.join(".mcp.json")
        } else {
            agent.join("mcp.json")
        };
    let manifest = paths.manifest(
        HarnessKind::Omp,
        &[mcp_file.as_path(), loader.path.as_path()],
    );
    if let Some(bytes) = read_native(&mcp_file)? {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("native Omp MCP configuration must be UTF-8"))?;
        let parsed = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
            .map_err(|_| anyhow::anyhow!("invalid native Omp MCP configuration"))?
            .value
            .context("empty native Omp MCP configuration")?;
        let parsed: serde_json::Value = parsed.into();
        let named = |key: &str| {
            parsed
                .get(key)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|names| names.iter().any(|name| name.as_str() == Some("bkpr")))
        };
        if named("disabledServers")
            || !named("enabledServers")
                && parsed
                    .pointer("/mcpServers/bkpr/enabled")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        {
            blockers.push("Omp's native MCP configuration disables the Bkpr server; enable it natively before installing or launching managed capture.".into());
        }
    }
    let mcp = json!({
        "type": "stdio", "command": paths.program,
        "args": ["capture", "installed-mcp", "--harness", "omp", "--installation", manifest]
    });
    let entries = ["type", "command", "args"]
        .into_iter()
        .map(|key| {
            Entry::value(
                &format!("mcp-{key}"),
                mcp_file.clone(),
                Format::Json,
                &["mcpServers", "bkpr", key],
                mcp[key].clone(),
            )
        })
        .collect();
    Ok(Definition { kind: HarnessKind::Omp, bundle, entries, files: vec![loader],
        removals: vec![Removal {file:mcp_file,format:Format::Json,keys:vec!["mcpServers".into(),"bkpr".into()],allowed:["enabled","timeout","requestIdFormat","instructions"].into_iter().map(String::from).collect()}],
        blockers, notes: vec![
            "Omp 18.19 native user/profile extensions and MCP; restart Omp to load. Native extension/MCP disable settings and tool approvals remain authoritative. Bkpr control skills are supplied only to managed launches and respect --no-skills.".into(),
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
    fn native_profile_precedence_keeps_the_loader_in_the_selected_account() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = paths(root.path());
        paths
            .environment
            .insert("PI_PROFILE".into(), "legacy".into());
        paths
            .environment
            .insert("OMP_PROFILE".into(), "review".into());
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), "/another-account".into());
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.home.join(".omp/profiles/review/agent")
        );
        paths.environment.insert("OMP_PROFILE".into(), "".into());
        paths.environment.insert(
            "PI_CODING_AGENT_DIR".into(),
            paths
                .home
                .join(".omp/profiles/legacy/agent")
                .into_os_string(),
        );
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.home.join(".omp/agent")
        );
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), "relative agent".into());
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.cwd.join("relative agent")
        );
        paths
            .environment
            .insert("OMP_PROFILE".into(), "../outside".into());
        assert!(agent_directory(&paths).is_err());
    }

    #[test]
    fn native_disabled_preferences_follow_project_and_explicit_overlay_precedence() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = paths(root.path());
        let agent = paths.home.join(".omp/agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(paths.cwd.join(".omp")).unwrap();
        std::fs::write(
            agent.join("config.yml"),
            "# Native user preferences\ndisabledExtensions: [extension-module:bkpr]\n",
        )
        .unwrap();
        assert!(!definition(&paths, &[]).unwrap().blockers.is_empty());
        std::fs::write(
            paths.cwd.join(".omp/config.yml"),
            "disabledExtensions: []\n",
        )
        .unwrap();
        assert!(definition(&paths, &[]).unwrap().blockers.is_empty());
        let overlay = paths.cwd.join("native override.yml");
        std::fs::write(&overlay, "disabledExtensions:\n  - extension-module:bkpr\n").unwrap();
        paths
            .environment
            .insert("PI_CONFIG_FILES".into(), overlay.into_os_string());
        assert!(!definition(&paths, &[]).unwrap().blockers.is_empty());
    }
}
