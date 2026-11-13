//! Launch-private OpenCode 2 server/TUI plugins and the canonical shared MCP.
use std::{env, ffi::OsString, fs, io::IsTerminal, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use bkpr_protocol::HarnessKind;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    Capture,
    assets::{private_directory, write_credentials, write_private},
    passthrough, run_managed, scrub,
};

fn interactive_arguments(arguments: &[OsString]) -> bool {
    let mut index = 0;
    let mut directory = false;
    while index < arguments.len() {
        let Some(argument) = arguments[index].to_str() else {
            return false;
        };
        if argument == "--" {
            return arguments.len() == index + 1 || (!directory && arguments.len() == index + 2);
        }
        let (option, inline) = argument
            .split_once('=')
            .map_or((argument, false), |(name, _)| (name, true));
        match option {
            "--session" | "-s" | "--prompt" => {
                if !inline {
                    index += 1;
                    if index == arguments.len() {
                        return false;
                    }
                }
            }
            "--standalone"
            | "--continue"
            | "-c"
            | "--auto"
            | "--yolo"
            | "--dangerously-skip-permissions"
            | "--print-logs"
                if !inline => {}
            _ if option.starts_with('-') => return false,
            _ if !directory => {
                // OpenCode 2.2.7 packages/cli/src/commands/commands.ts:
                // only native root commands/aliases opt out; other words
                // remain directory positionals (including former V1 commands).
                if matches!(
                    argument,
                    "upgrade"
                        | "update"
                        | "uninstall"
                        | "acp"
                        | "api"
                        | "debug"
                        | "auth"
                        | "mcp"
                        | "plugin"
                        | "models"
                        | "stats"
                        | "mini"
                        | "run"
                        | "session"
                        | "service"
                        | "reload"
                        | "pair"
                        | "serve"
                ) {
                    return false;
                }
                directory = true;
            }
            _ => return false,
        }
        index += 1;
    }
    true
}

fn recovery_arguments(arguments: Vec<OsString>) -> Result<(Vec<OsString>, Option<String>)> {
    if arguments
        .first()
        .is_none_or(|argument| argument != "--bkpr-resume")
    {
        return Ok((arguments, None));
    }
    if arguments.len() != 2 {
        bail!("exact OpenCode recovery requires only its native ID, without startup overrides");
    }
    let id = arguments
        .into_iter()
        .nth(1)
        .and_then(|value| value.into_string().ok())
        .context("exact OpenCode recovery requires a UTF-8 native ID")?;
    super::validate_text(&id, 256, "exact native recovery ID")?;
    Ok((Vec::new(), Some(id)))
}

fn config_object(text: &str) -> Result<jsonc_parser::ast::Object<'_>> {
    let options = jsonc_parser::ParseOptions {
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
    let value = jsonc_parser::parse_to_ast(text, &Default::default(), &options)
        .map_err(|_| {
            anyhow::anyhow!("invalid OpenCode launch configuration (JSONC object required)")
        })?
        .value;
    match value {
        Some(jsonc_parser::ast::Value::Object(object)) => Ok(object),
        _ => bail!("OpenCode launch configuration must be an object"),
    }
}

fn config(text: &str) -> Result<Value> {
    Ok(jsonc_parser::ast::Value::Object(config_object(text)?).into())
}

fn environment_config(name: &str) -> Result<Value> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => config(&value),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(json!({})),
        Err(_) => bail!("OpenCode launch configuration must be UTF-8"),
    }
}

fn append_plugin(config: &mut Value, plugin: Value) -> Result<()> {
    let plugins = config
        .as_object_mut()
        .expect("configuration object")
        .entry("plugins")
        .or_insert_with(|| json!([]));
    plugins
        .as_array_mut()
        .context("OpenCode plugins must be an array")?
        .push(plugin);
    Ok(())
}

fn cli_config_directory() -> Result<PathBuf> {
    if let Some(path) = env::var_os("OPENCODE_CONFIG_DIR") {
        return Ok(path.into());
    }
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("opencode"));
    }
    Ok(
        PathBuf::from(env::var_os("HOME").context("OpenCode requires a home directory")?)
            .join(".config/opencode"),
    )
}

// Native source ledger (OpenCode 2.2.7):
// - packages/cli/src/config/config.ts:31-34,64-78,118-138: file then env;
//   objects merge, arrays replace.
// - packages/cli/src/config/migrate.ts:37-100,160-166,194: an existing
//   cli.json suppresses tui.json migration; tuples become package/options,
//   followed by plugin_enabled directives. kv.json contributes no plugins.
// - packages/tui/src/config/v1/index.tsx:18-19,66-67: legacy plugin shapes.
// There is no read-only CLI-config query: debug config queries server config;
// plugin list calls config.get(), which persists migration and resolves packages.
// Extract only plugins without invoking that engine or changing native files.
fn migrated_cli_plugins(text: &str) -> Result<Value> {
    let mut specifications = None;
    let mut enablement = None;
    for property in config_object(text)?.properties {
        match property.name.as_str() {
            "plugin" => specifications = Some(property.value),
            "plugin_enabled" => enablement = Some(property.value),
            _ => {}
        }
    }
    let mut plugins = Vec::new();
    if let Some(value) = specifications {
        let jsonc_parser::ast::Value::Array(array) = value else {
            bail!("OpenCode legacy plugin must be an array");
        };
        for plugin in array.elements {
            let plugin: Value = plugin.into();
            match plugin {
                Value::String(_) => plugins.push(plugin),
                Value::Array(mut tuple)
                    if tuple.len() == 2 && tuple[0].is_string() && tuple[1].is_object() =>
                {
                    let options = tuple.pop().expect("plugin options");
                    let package = tuple.pop().expect("plugin package");
                    plugins.push(json!({"package": package, "options": options}));
                }
                _ => bail!("invalid OpenCode legacy plugin declaration"),
            }
        }
    }
    if let Some(value) = enablement {
        let enabled = value
            .as_object()
            .context("OpenCode legacy plugin_enabled must be an object")?;
        // Match Object.entries: first insertion order, last duplicate value,
        // with canonical array-index keys before other keys. A serde_json map
        // would sort wildcard directives and change native enablement precedence.
        let mut entries: Vec<(&str, bool)> = Vec::with_capacity(enabled.properties.len());
        for property in &enabled.properties {
            let name = property.name.as_str();
            let enabled = property
                .value
                .as_boolean_lit()
                .context("OpenCode legacy plugin_enabled values must be boolean")?
                .value;
            if let Some(entry) = entries.iter_mut().find(|entry| entry.0 == name) {
                entry.1 = enabled;
            } else {
                entries.push((name, enabled));
            }
        }
        entries.sort_by_key(|(name, _)| {
            name.parse::<u32>()
                .ok()
                .filter(|index| {
                    *index != u32::MAX
                        && (*name == "0" || (!name.starts_with('0') && !name.starts_with('+')))
                })
                .map_or((true, 0), |index| (false, index))
        });
        plugins.extend(entries.into_iter().map(|(name, enabled)| {
            Value::String(if enabled {
                name.into()
            } else {
                format!("-{name}")
            })
        }));
    }
    Ok(Value::Array(plugins))
}

fn preserve_cli_plugins(cli: &mut Value, native: Option<&str>, legacy: Option<&str>) -> Result<()> {
    if cli.get("plugins").is_some() {
        return Ok(());
    }
    if let Some(native) = native {
        if let Some(plugins) = config(native)?.get_mut("plugins") {
            cli["plugins"] = plugins.take();
        }
    } else if let Some(legacy) = legacy {
        cli["plugins"] = migrated_cli_plugins(legacy)?;
    }
    Ok(())
}

fn read_native_config(path: &std::path::Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read native CLI plugin settings"),
    }
}

fn launch_configs(capture: &Capture, directory: &std::path::Path) -> Result<(Value, Value)> {
    let credentials = directory.join("credentials.json");
    let plugin =
        json!({"package": directory.join("opencode"), "options": {"captureFile": credentials}});
    let mut server = environment_config("OPENCODE_CONFIG_CONTENT")?;
    append_plugin(&mut server, plugin.clone())?;
    let mcp = server
        .as_object_mut()
        .expect("server object")
        .entry("mcp")
        .or_insert_with(|| json!({}));
    let servers = mcp
        .as_object_mut()
        .context("OpenCode mcp must be an object")?
        .entry("servers")
        .or_insert_with(|| json!({}));
    servers.as_object_mut().context("OpenCode mcp.servers must be an object")?.insert("bkpr".into(), json!({
        "type":"local", "codemode":false, "command":[capture.program, "capture", "mcp", "--harness", "opencode", "--capture-file", credentials]
    }));
    let mut cli = environment_config("OPENCODE_CLI_CONFIG_CONTENT")?;
    // The launch-private array must retain native plugins before appending ours.
    // Do not read lower-precedence files when the environment replaces the list.
    if cli.get("plugins").is_none() {
        let directory = cli_config_directory()?;
        let native = read_native_config(&directory.join("cli.json"))?;
        let legacy = if native.is_none() {
            read_native_config(&directory.join("tui.json"))?
        } else {
            None
        };
        preserve_cli_plugins(&mut cli, native.as_deref(), legacy.as_deref())?;
    }
    append_plugin(&mut cli, plugin)?;
    Ok((server, cli))
}

fn package(capture: &Capture) -> Result<TempDir> {
    let directory = private_directory("bkpr-opencode-")?;
    write_credentials(capture, &directory.path().join("credentials.json"))?;
    fs::create_dir(directory.path().join("opencode"))?;
    fs::create_dir(directory.path().join("shared"))?;
    write_private(
        &directory.path().join("package.json"),
        br#"{"name":"bkpr-opencode-native","private":true,"type":"module"}"#,
    )?;
    for (path, bytes) in [
        (
            "opencode/index.ts",
            include_bytes!("../../../../integration/opencode/index.ts").as_slice(),
        ),
        (
            "opencode/tui.ts",
            include_bytes!("../../../../integration/opencode/tui.ts").as_slice(),
        ),
        (
            "opencode/rpc.ts",
            include_bytes!("../../../../integration/opencode/rpc.ts").as_slice(),
        ),
        (
            "opencode/session.ts",
            include_bytes!("../../../../integration/opencode/session.ts").as_slice(),
        ),
        (
            "shared/native.ts",
            include_bytes!("../../../../integration/shared/native.ts").as_slice(),
        ),
    ] {
        write_private(&directory.path().join(path), bytes)?;
    }
    Ok(directory)
}

pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    let (arguments, required) = recovery_arguments(arguments)?;
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !interactive_arguments(&arguments)
    {
        if required.is_some() {
            bail!("exact OpenCode recovery requires the managed native interactive mode");
        }
        return passthrough(&executable, arguments);
    }
    let capture = Capture::begin(HarnessKind::Opencode)?;
    let package = package(&capture)?;
    let (mut server, mut cli) = launch_configs(&capture, package.path())?;
    if let Some(required) = required.as_deref() {
        for config in [&mut server, &mut cli] {
            let plugin = config["plugins"]
                .as_array_mut()
                .expect("configured plugins")
                .last_mut()
                .expect("native plugin");
            plugin["options"]["requiredSession"] = json!({"id": required});
        }
    }
    let mut command = Command::new(executable);
    scrub(&mut command);
    // Credentials live only in the private file, never the native server or
    // its tools' inherited environment. Only callback/MCP children claim them.
    command.env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&server)?);
    command.env("OPENCODE_CLI_CONFIG_CONTENT", serde_json::to_string(&cli)?);
    if !arguments.iter().any(|argument| argument == "--standalone") {
        command.arg("--standalone");
    }
    command.args(arguments);
    let result = run_managed(&mut command, &capture);
    drop(package);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn exact_recovery_uses_home_without_changing_native_prompt_values() {
        let (native, required) =
            recovery_arguments(args(&["--bkpr-resume", "ses_existing"])).unwrap();
        assert!(native.is_empty());
        assert_eq!(required.as_deref(), Some("ses_existing"));
        assert!(
            recovery_arguments(args(&["--bkpr-resume", "ses_existing", "--prompt", "text"]))
                .is_err()
        );
        for values in [
            vec!["--prompt", "--bkpr-resume"],
            vec!["--session", "ses_create_if_missing"],
            vec!["api", "session.get", "--data", "--bkpr-resume"],
            vec!["--", "--bkpr-resume"],
        ] {
            let original = args(&values);
            let (native, required) = recovery_arguments(original.clone()).unwrap();
            assert_eq!(native, original);
            assert!(required.is_none());
        }
    }

    #[test]
    fn native_external_and_noninteractive_modes_are_not_managed() {
        for values in [
            vec!["--server", "http://localhost:1234"],
            vec!["--server=http://localhost:1234"],
            vec!["--help"],
            vec!["--version"],
            vec!["run", "prompt"],
            vec!["service", "start"],
            vec!["attach", "http://localhost:1234"],
            vec!["--unknown"],
        ] {
            assert!(!interactive_arguments(&args(&values)), "{values:?}");
        }
        for values in [
            vec![],
            vec!["--session", "ses_exact"],
            vec!["--prompt", "--server"],
            vec!["--standalone", "--continue"],
            vec!["--", "--server"],
        ] {
            assert!(interactive_arguments(&args(&values)), "{values:?}");
        }
    }

    #[test]
    fn every_native_v2_command_bypasses_managed_tui_plugins() {
        for command in [
            "upgrade",
            "update",
            "uninstall",
            "acp",
            "api",
            "debug",
            "auth",
            "mcp",
            "plugin",
            "models",
            "stats",
            "mini",
            "run",
            "session",
            "service",
            "reload",
            "pair",
            "serve",
        ] {
            assert!(!interactive_arguments(&args(&[command])), "{command}");
            assert!(
                !interactive_arguments(&args(&["--print-logs", command])),
                "{command}"
            );
            for values in [
                vec!["--prompt", command],
                vec!["--session", command],
                vec!["--", command],
            ] {
                let original = args(&values);
                let (native, required) = recovery_arguments(original.clone()).unwrap();
                assert_eq!(native, original);
                assert!(required.is_none());
                assert!(interactive_arguments(&native), "{values:?}");
            }
            assert!(interactive_arguments(&args(&[&format!("./{command}")])));
            assert!(interactive_arguments(&args(&[&format!(
                "--prompt={command}"
            )])));
        }
    }

    #[test]
    fn native_directory_positionals_are_not_guessed_from_command_like_words() {
        for values in [
            vec!["project"],
            vec!["attach"],
            vec!["export"],
            vec!["import"],
            vec!["completion"],
            vec!["project", "--prompt", "reload"],
            vec!["--prompt", "--", "project"],
            vec!["--", "--literal-directory"],
            vec!["project", "--"],
        ] {
            assert!(interactive_arguments(&args(&values)), "{values:?}");
        }
        for values in [
            vec!["project", "another"],
            vec!["project", "--", "another"],
            vec!["--", "project", "another"],
            vec!["--prompt"],
            vec!["--session"],
        ] {
            assert!(!interactive_arguments(&args(&values)), "{values:?}");
        }
    }

    #[test]
    fn absent_cli_json_preserves_migrated_plugins_options_and_disabled_directives() {
        let legacy = r#"{
            // Native V1 declarations precede enablement directives.
            "plugin":["native",["configured",{"nested":{"value":42},"enabled":false}]],
            "plugin_enabled":{"*":false,"native":true,"configured":false},
            "theme":"native-theme",
        }"#;
        let mut cli = json!({"theme":{"name":"environment-theme"}});
        preserve_cli_plugins(&mut cli, None, Some(legacy)).unwrap();
        append_plugin(&mut cli, json!({"package":"/private/bkpr"})).unwrap();
        assert_eq!(
            cli,
            json!({
                "theme":{"name":"environment-theme"},
                "plugins":[
                    "native",
                    {"package":"configured","options":{"nested":{"value":42},"enabled":false}},
                    "-*","native","-configured",
                    {"package":"/private/bkpr"}
                ]
            })
        );
    }

    #[test]
    fn existing_cli_json_suppresses_legacy_migration_even_without_plugins() {
        let legacy = r#"{"plugin":["legacy"],"plugin_enabled":{"*":false}}"#;
        for native in [r#"{}"#, r#"{"theme":{"name":"native-theme"}}"#] {
            let mut cli = json!({});
            preserve_cli_plugins(&mut cli, Some(native), Some(legacy)).unwrap();
            append_plugin(&mut cli, json!("bkpr")).unwrap();
            assert_eq!(cli, json!({"plugins":["bkpr"]}));
        }
        let mut cli = json!({});
        preserve_cli_plugins(
            &mut cli,
            Some(r#"{"plugins":["-*",{"package":"native","options":{"value":1}}]}"#),
            Some("invalid ignored legacy"),
        )
        .unwrap();
        append_plugin(&mut cli, json!("bkpr")).unwrap();
        assert_eq!(
            cli["plugins"],
            json!(["-*",{"package":"native","options":{"value":1}},"bkpr"])
        );
    }

    #[test]
    fn environment_cli_plugin_arrays_replace_files_including_explicit_empty_array() {
        for plugins in [
            json!([]),
            json!(["-*",{"package":"environment","options":{"enabled":false}}]),
        ] {
            let mut cli = json!({"plugins":plugins});
            let mut expected = cli.clone();
            preserve_cli_plugins(
                &mut cli,
                Some("invalid overridden cli file"),
                Some("invalid overridden legacy file"),
            )
            .unwrap();
            append_plugin(&mut cli, json!("bkpr")).unwrap();
            expected["plugins"]
                .as_array_mut()
                .unwrap()
                .push(json!("bkpr"));
            assert_eq!(cli, expected);
        }
        let mut cli = json!({});
        preserve_cli_plugins(&mut cli, None, None).unwrap();
        append_plugin(&mut cli, json!("bkpr")).unwrap();
        assert_eq!(cli, json!({"plugins":["bkpr"]}));
    }

    #[test]
    fn migrated_enablement_retains_native_object_entry_order_and_duplicate_precedence() {
        assert_eq!(
            migrated_cli_plugins(
                r#"{
                    "plugin":["ignored duplicate"],
                    "plugin":["retained"],
                    "plugin_enabled":{
                        "z":true,"*":false,"z":false,"01":true,"10":true,"2":false,"a":true
                    }
                }"#
            )
            .unwrap(),
            json!(["retained", "-2", "10", "-z", "-*", "01", "a"])
        );
    }

    #[test]
    fn launch_overlay_retains_jsonc_values_and_plugin_options() {
        let mut value = config(r#"{/* native */ "model":"custom/model", "plugins":[{"package":"native", "options":{"enabled":true}}], "instructions":["{env:INSTRUCTIONS}"],}"#).unwrap();
        append_plugin(
            &mut value,
            json!({"package":"/private/bkpr", "options":{"captureFile":"/private/credentials"}}),
        )
        .unwrap();
        assert_eq!(value["model"], "custom/model");
        assert_eq!(value["instructions"][0], "{env:INSTRUCTIONS}");
        assert_eq!(value["plugins"][0]["options"]["enabled"], true);
        assert_eq!(value["plugins"].as_array().unwrap().len(), 2);
        for text in ["[]", "{bad:1}", "{\"bad\":'value'}", "{\"bad\":NaN}"] {
            assert!(config(text).is_err());
        }
    }
}
