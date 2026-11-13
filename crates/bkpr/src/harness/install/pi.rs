//! Pi's native auto-discovered, launch-authenticated extension.
use anyhow::{Context, Result};
use bkpr_protocol::HarnessKind;

use super::types::{Asset, Bundle, Definition, OwnedFile, Paths};

fn agent_directory(paths: &Paths) -> Result<std::path::PathBuf> {
    // Pi 1.5.2 config.ts / utils/paths.ts: tilde and file URLs are accepted;
    // relative overrides are resolved by the native loader against cwd.
    let Some(value) = paths
        .environment
        .get(std::ffi::OsStr::new("PI_CODING_AGENT_DIR"))
        .filter(|value| !value.is_empty())
    else {
        return Ok(paths.home.join(".pi/agent"));
    };
    let value = value.to_str().context("Pi agent directory must be UTF-8")?;
    if value == "~" {
        return Ok(paths.home.clone());
    }
    if let Some(value) = value.strip_prefix("~/") {
        return Ok(paths.home.join(value));
    }
    if value.starts_with("file://") {
        return url::Url::parse(value)?
            .to_file_path()
            .map_err(|()| anyhow::anyhow!("invalid Pi agent file URL"));
    }
    paths.env_path("PI_CODING_AGENT_DIR", paths.home.join(".pi/agent"))
}

/// Plan Pi's native extension; MCP is registered only after managed activation.
pub(super) fn definition(paths: &Paths) -> Result<Definition> {
    let agent = agent_directory(paths)?;
    let bundle = Bundle::new(paths, HarnessKind::Pi, vec![
        Asset::borrowed("pi/index.ts", include_bytes!("../../../../../integration/pi/index.ts")),
        Asset::borrowed("shared/native.ts", include_bytes!("../../../../../integration/shared/native.ts")),
        Asset::borrowed("package.json", br#"{"name":"bkpr-pi-native","private":true,"type":"module","pi":{"extensions":["pi/index.ts"]}}"#),
    ])?;
    let source = url::Url::from_file_path(bundle.root.join("pi/index.ts"))
        .map_err(|()| anyhow::anyhow!("Pi native extension path must be absolute"))?;
    let loader = OwnedFile::text(
        "extension",
        agent.join("extensions/bkpr.ts"),
        format!(
            "export {{ default }} from {};\n",
            serde_json::to_string(source.as_str())?
        ),
    );
    Ok(Definition { kind: HarnessKind::Pi, bundle, entries: vec![], files: vec![loader], removals: vec![], blockers: vec![], notes: vec![
        "Pi 1.5.2 native extension discovery; restart or /reload to load. MCP is registered through Pi's native extension API only during a managed launch. File-configured same-name MCP, native extension/MCP opt-outs, trust and tool approvals remain authoritative.".into(),
    ] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agent_overrides_preserve_tilde_relative_and_encoded_file_paths() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = Paths {
            home: root.path().join("home"),
            data: root.path().join("data"),
            program: root.path().join("bin/bkpr"),
            cwd: root.path().join("project"),
            environment: Default::default(),
        };
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), "~/native agent".into());
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.home.join("native agent")
        );
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), "relative agent".into());
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.cwd.join("relative agent")
        );
        let target = root.path().join("native # % agent");
        let uri = url::Url::from_directory_path(&target).unwrap();
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), uri.as_str().into());
        assert_eq!(agent_directory(&paths).unwrap(), target);
        paths
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), "".into());
        assert_eq!(
            agent_directory(&paths).unwrap(),
            paths.home.join(".pi/agent")
        );
    }
}
