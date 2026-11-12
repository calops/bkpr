//! Launch-local OpenCode TUI integration without persistent configuration changes.
use super::{Capture, agent, assets, passthrough, run_managed, scrub};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::HarnessKind;
use jsonc_parser::ParseOptions;
use serde_json::{Map, Value, json};
use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    process::Command,
};

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

fn interactive_arguments(arguments: &[OsString]) -> bool {
    let mut positional = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(argument) = argument.to_str() else {
            return false;
        };
        if argument == "--" {
            return arguments.len() <= usize::from(!positional);
        }
        let (option, inline) = argument
            .split_once('=')
            .map_or((argument, false), |(key, _)| (key, true));
        match option {
            "--model" | "-m" | "--session" | "-s" | "--prompt" | "--agent" | "--log-level" => {
                if !inline && arguments.next().is_none() {
                    return false;
                }
            }
            "--continue"
            | "-c"
            | "--fork"
            | "--print-logs"
            | "--auto"
            | "--yolo"
            | "--dangerously-skip-permissions"
                if !inline => {}
            _ if option.starts_with('-') => return false,
            _ if !positional => {
                if matches!(
                    argument,
                    "acp"
                        | "account"
                        | "agent"
                        | "attach"
                        | "auth"
                        | "completion"
                        | "db"
                        | "debug"
                        | "export"
                        | "generate"
                        | "github"
                        | "import"
                        | "mcp"
                        | "mini"
                        | "models"
                        | "plug"
                        | "plugin"
                        | "pr"
                        | "providers"
                        | "run"
                        | "serve"
                        | "service"
                        | "session"
                        | "stats"
                        | "uninstall"
                        | "upgrade"
                        | "web"
                ) {
                    return false;
                }
                positional = true;
            }
            _ => return false,
        }
    }
    true
}

fn parse_config(text: &str) -> Result<Map<String, Value>> {
    let options = ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    };
    let value: Value = jsonc_parser::parse_to_serde_value(text, &options).map_err(|_| {
        anyhow::anyhow!("OpenCode inline configuration must be a valid JSONC object")
    })?;
    match value {
        Value::Object(object) => Ok(object),
        _ => bail!("OpenCode inline configuration must be a JSONC object"),
    }
}

fn server_overlay(
    mut base: Map<String, Value>,
    program: &Path,
    credentials: &Path,
    guide: &Path,
    plugin: &Path,
) -> Result<Value> {
    let servers = base
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("OpenCode mcp configuration must be an object")?;
    servers.insert(
        "bkpr".into(),
        json!({
            "type":"local", "enabled":true,
            "command":[program,"capture","mcp","--harness","opencode","--capture-file",credentials]
        }),
    );
    base.entry("instructions")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("OpenCode instructions configuration must be an array")?
        .push(json!(guide));
    base.entry("plugin")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("OpenCode server plugins configuration must be an array")?
        .push(json!([plugin,{"captureFile":credentials}]));
    Ok(Value::Object(base))
}

fn resolve_plugin_origin(spec: &mut Value, directory: &Path) -> Result<()> {
    let spec = if let Value::Array(tuple) = spec {
        tuple
            .first_mut()
            .context("OpenCode plugin tuple must have a target")?
    } else {
        spec
    };
    let path = spec
        .as_str()
        .context("OpenCode plugin target must be a string")?;
    if path.starts_with("./") || path.starts_with("../") {
        *spec = json!(directory.join(path));
    }
    Ok(())
}

fn rebase_file_references(value: &mut Value, origin: &Path) -> Result<()> {
    match value {
        Value::Array(values) => {
            for value in values {
                rebase_file_references(value, origin)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                ensure!(
                    !key.contains("{file:") && !key.contains("{env:"),
                    "dynamic configuration keys require native loading"
                );
                rebase_file_references(value, origin)?;
            }
        }
        Value::String(text) if text.contains("{file:") => {
            let mut output = String::with_capacity(text.len() + origin.as_os_str().len());
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{file:") {
                output.push_str(&rest[..start]);
                let token = &rest[start + 6..];
                let Some(end) = token.find('}') else {
                    output.push_str(&rest[start..]);
                    rest = "";
                    break;
                };
                let file = &token[..end];
                ensure!(
                    !file.contains("{env:"),
                    "dynamic file references require native loading"
                );
                output.push_str("{file:");
                if Path::new(file).is_absolute() || file.starts_with("~/") {
                    output.push_str(file);
                } else {
                    let path = origin.join(file);
                    let path = path
                        .to_str()
                        .context("configuration resource path must be UTF-8")?;
                    ensure!(
                        !path
                            .chars()
                            .any(|ch| matches!(ch, '"' | '\\' | '}') || ch.is_control()),
                        "configuration resource path requires native loading"
                    );
                    output.push_str(path);
                }
                output.push('}');
                rest = &token[end + 1..];
            }
            output.push_str(rest);
            *text = output;
        }
        _ => {}
    }
    Ok(())
}

fn normalize_tui(
    mut base: Map<String, Value>,
    origin: Option<&Path>,
) -> Result<Map<String, Value>> {
    if let Some(Value::Object(mut nested)) = base.remove("tui") {
        nested.extend(base);
        base = nested;
    }
    if let Some(origin) = origin {
        for (key, value) in &mut base {
            ensure!(
                !key.contains("{file:") && !key.contains("{env:"),
                "dynamic configuration keys require native loading"
            );
            rebase_file_references(value, origin)?;
        }
        if let Some(plugins) = base.get_mut("plugin") {
            for spec in plugins
                .as_array_mut()
                .context("OpenCode TUI plugins must be an array")?
            {
                let target = spec
                    .as_array()
                    .and_then(|tuple| tuple.first())
                    .unwrap_or(spec);
                ensure!(
                    target
                        .as_str()
                        .is_some_and(|value| !value.contains("{env:") && !value.contains("{file:")),
                    "dynamic plugin targets require native loading"
                );
                resolve_plugin_origin(spec, origin)?;
            }
        }
        if let Some(sounds) = base
            .get_mut("attention")
            .and_then(|value| value.get_mut("sounds"))
            .and_then(Value::as_object_mut)
        {
            for value in sounds.values_mut() {
                let Some(file) = value.as_str() else { continue };
                ensure!(
                    !file.contains("{env:") && !file.contains("{file:"),
                    "dynamic sound resources require native loading"
                );
                if !file.is_empty() && !Path::new(file).is_absolute() && !file.starts_with("~/") {
                    *value = json!(origin.join(file));
                }
            }
        }
    }
    Ok(base)
}

fn tui_overlay(mut base: Map<String, Value>, plugin: &Path, credentials: &Path) -> Result<Value> {
    let plugins = base
        .entry("plugin")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("OpenCode TUI plugins configuration must be an array")?;
    plugins.push(json!([plugin,{"captureFile":credentials}]));
    Ok(Value::Object(base))
}

fn native_directory(arguments: &[OsString]) -> Result<PathBuf> {
    let cwd = env::current_dir()?;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(value) = argument.to_str() else {
            continue;
        };
        let (option, inline) = value
            .split_once('=')
            .map_or((value, false), |(key, _)| (key, true));
        if matches!(
            option,
            "--model" | "-m" | "--session" | "-s" | "--prompt" | "--agent" | "--log-level"
        ) {
            if !inline {
                arguments.next();
            }
            continue;
        }
        if value == "--" {
            continue;
        }
        if !value.starts_with('-') {
            let base = env::var_os("PWD").map(PathBuf::from).unwrap_or(cwd);
            return Ok(base.join(argument));
        }
    }
    Ok(cwd)
}

fn tui_base(arguments: &[OsString]) -> Result<Map<String, Value>> {
    let Some(path) = env::var_os("OPENCODE_TUI_CONFIG") else {
        return Ok(Map::new());
    };
    let path = native_directory(arguments)?.join(path);
    let file = File::open(&path).context("open explicit OpenCode TUI configuration")?;
    ensure!(
        file.metadata()?.len() <= MAX_CONFIG_BYTES,
        "OpenCode TUI configuration exceeds integration limit"
    );
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .context("read explicit OpenCode TUI configuration")?;
    ensure!(
        text.len() as u64 <= MAX_CONFIG_BYTES,
        "OpenCode TUI configuration exceeds integration limit"
    );
    let parent = path
        .parent()
        .context("OpenCode TUI configuration needs a directory")?
        .to_owned();
    normalize_tui(parse_config(&text)?, Some(&parent))
}

/// Launch an interactive OpenCode TUI with launch-scoped native integration.
pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    let pure = env::var_os("OPENCODE_PURE").is_some_and(|value| value == "1" || value == "true");
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !interactive_arguments(&arguments)
        || pure
    {
        return passthrough(&executable, arguments);
    }
    let configuration = (|| -> Result<_> {
        let base = match env::var("OPENCODE_CONFIG_CONTENT") {
            Ok(text) => parse_config(&text)?,
            Err(env::VarError::NotPresent) => Map::new(),
            Err(_) => bail!("OpenCode inline configuration must be UTF-8"),
        };
        Ok((base, tui_base(&arguments)?))
    })();
    let (base, tui) = match configuration {
        Ok(configuration) => configuration,
        Err(_) => {
            eprintln!(
                "bkpr: OpenCode configuration requires native loading; launching unchanged without Bkpr integration"
            );
            return passthrough(&executable, arguments);
        }
    };
    let capture = Capture::begin(HarnessKind::Opencode)?;
    let package = assets::private_directory("bkpr-opencode-")?;
    let directory = package.path();
    fs::create_dir(directory.join("opencode"))?;
    fs::create_dir(directory.join("shared"))?;
    let plugin = directory.join("opencode/index.ts");
    assets::write_private(
        &plugin,
        include_bytes!("../../../../integration/opencode/index.ts"),
    )?;
    let server = directory.join("opencode/server.ts");
    assets::write_private(
        &server,
        include_bytes!("../../../../integration/opencode/server.ts"),
    )?;
    assets::write_private(
        &directory.join("shared/native.ts"),
        include_bytes!("../../../../integration/shared/native.ts"),
    )?;
    let credentials = directory.join("capture.json");
    assets::write_credentials(&capture, &credentials)?;
    let guide = directory.join("bkpr-instructions.md");
    assets::write_private(&guide, agent::INSTRUCTIONS.as_bytes())?;
    let overlay = server_overlay(base, &capture.program, &credentials, &guide, &server)?;
    let tui = tui_overlay(tui, &plugin, &credentials)?;
    let tui_file = directory.join("tui.json");
    assets::write_private(&tui_file, &serde_json::to_vec(&tui)?)?;
    let mut command = Command::new(executable);
    scrub(&mut command);
    command
        .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&overlay)?)
        .env("OPENCODE_TUI_CONFIG", &tui_file)
        .args(arguments);
    let result = run_managed(&mut command, &capture);
    drop(package);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_overlay_keeps_native_configuration_and_rejects_non_json_extensions() {
        let base = parse_config(
            r#"{// user configuration
          "model":"local/custom", "mcp":{"other":{"type":"local","command":["other"]}},
          "instructions":["existing.md"],
        }"#,
        )
        .unwrap();
        let merged = server_overlay(
            base,
            Path::new("/bkpr"),
            Path::new("/private/capture.json"),
            Path::new("/private/guide.md"),
            Path::new("/private/opencode/server.ts"),
        )
        .unwrap();
        assert_eq!(merged["model"], "local/custom");
        assert_eq!(merged["mcp"]["other"]["command"], json!(["other"]));
        assert_eq!(
            merged["instructions"],
            json!(["existing.md", "/private/guide.md"])
        );
        assert!(parse_config("{unquoted:1}").is_err());
        assert!(parse_config("{'single':1}").is_err());
    }

    #[test]
    fn relocated_tui_preserves_nested_precedence_and_resource_origins() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("custom.ts"), "custom plugin").unwrap();
        fs::write(directory.path().join("done.wav"), "sound bytes").unwrap();
        fs::write(directory.path().join("theme-name.txt"), "user-theme").unwrap();
        let base = parse_config(
            r#"{
            "tui":{"theme":"nested","plugin":[["./custom.ts",{"flag":true}]],
                "attention":{"sounds":{"done":"done.wav"}}},
            "theme":"{file:theme-name.txt}"
        }"#,
        )
        .unwrap();
        let merged = normalize_tui(base, Some(directory.path())).unwrap();
        let plugin = Path::new(merged["plugin"][0][0].as_str().unwrap());
        assert_eq!(fs::read_to_string(plugin).unwrap(), "custom plugin");
        assert_eq!(merged["plugin"][0][1]["flag"], true);
        let sound = Path::new(merged["attention"]["sounds"]["done"].as_str().unwrap());
        assert_eq!(fs::read_to_string(sound).unwrap(), "sound bytes");
        let reference = merged["theme"].as_str().unwrap();
        let theme = reference
            .strip_prefix("{file:")
            .unwrap()
            .strip_suffix('}')
            .unwrap();
        assert_eq!(fs::read_to_string(theme).unwrap(), "user-theme");
        assert!(!merged.contains_key("tui"));
    }

    #[test]
    fn runtime_dependent_resource_origins_require_unchanged_native_loading() {
        for text in [
            r#"{"plugin":["{env:CUSTOM_PLUGIN}"]}"#,
            r#"{"attention":{"sounds":{"done":"{env:CUSTOM_SOUND}"}}}"#,
            r#"{"theme":"{file:{env:CUSTOM_THEME_FILE}}"}"#,
        ] {
            assert!(
                normalize_tui(parse_config(text).unwrap(), Some(Path::new("/config"))).is_err()
            );
        }
    }

    #[test]
    fn noninteractive_external_and_disabled_plugin_modes_do_not_capture() {
        for args in [
            vec!["--help"],
            vec!["--version"],
            vec!["run", "hello"],
            vec!["attach", "http://host"],
            vec!["--port", "0"],
            vec!["--mini"],
            vec!["--future-option"],
        ] {
            assert!(!interactive_arguments(
                &args.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
        for args in [
            vec!["--session", "ses_native"],
            vec!["--continue", "--fork"],
            vec!["--model", "local/native", "project"],
        ] {
            assert!(interactive_arguments(
                &args.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
    }
}
