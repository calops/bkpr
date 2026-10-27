//! Explicit, opt-in persistent harness configuration.
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Harness whose user-level MCP configuration should be updated.
    #[arg(value_enum)]
    target: Target,
    /// Explain the change without writing configuration or invoking MCP add.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Target {
    #[value(alias = "antigravity")]
    Agy,
}

pub(crate) fn run(args: Args) -> Result<()> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .context("install requires an absolute HOME to locate the AGY user configuration")?;
    let path = env::var_os("PATH").unwrap_or_default();
    let cwd = env::current_dir().context("locate the current directory")?;
    let outcome = match args.target {
        Target::Agy => install_agy(&home, &path, &cwd, args.dry_run, || {
            crate::harness::resolve("agy", None)
                .or_else(|_| crate::harness::resolve("antigravity", None))
        })?,
    };
    println!("{outcome}");
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Configured(PathBuf),
    AlreadyConfigured(PathBuf),
    DryRun(PathBuf),
}

impl fmt::Display for Outcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configured(path) => write!(
                formatter,
                "Configured AGY MCP server bkpr in {} (bkpr relay-mcp). Start AGY through bkpr run agy for launch authentication; this does not establish receiver readiness.",
                path.display()
            ),
            Self::AlreadyConfigured(path) => write!(
                formatter,
                "AGY MCP server bkpr is already configured in {}; no changes made.",
                path.display()
            ),
            Self::DryRun(path) => write!(
                formatter,
                "Would run agy mcp add bkpr bkpr relay-mcp to configure {}; no changes made. Only the command and arguments are persisted, not launch credentials.",
                path.display()
            ),
        }
    }
}

struct Config {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    value: Value,
}

impl Config {
    fn inspect(path: PathBuf) -> Result<Self> {
        let bytes = read_config(&path)?;
        let value = match &bytes {
            Some(bytes) => serde_json::from_slice::<UniqueValue>(bytes)
                .with_context(|| {
                    format!(
                        "refusing to change {}: expected unambiguous JSON (AGY does not accept JSONC comments or trailing commas)",
                        path.display()
                    )
                })?
                .0,
            None => json!({}),
        };
        let object = value.as_object().with_context(|| {
            format!(
                "refusing to change {}: expected a JSON object",
                path.display()
            )
        })?;
        if let Some(servers) = object.get("mcpServers")
            && !servers.is_object()
        {
            bail!(
                "refusing to change {}: mcpServers must be a JSON object",
                path.display()
            );
        }
        Ok(Self { path, bytes, value })
    }

    fn already_configured(&self) -> Result<bool> {
        let Some(entry) = self
            .value
            .get("mcpServers")
            .and_then(|servers| servers.get("bkpr"))
        else {
            return Ok(false);
        };
        let mut enabled_entry = desired_entry();
        if *entry == enabled_entry {
            return Ok(true);
        }
        enabled_entry.as_object_mut().unwrap().remove("disabled");
        if *entry == enabled_entry {
            return Ok(true);
        }
        bail!(
            "refusing to overwrite the existing conflicting or disabled bkpr MCP server in {}; review and remove or update that entry explicitly, then rerun bkpr install agy",
            self.path.display()
        )
    }

    fn expected_after_add(&self) -> Value {
        let mut expected = self.value.clone();
        expected
            .as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap()
            .insert("bkpr".to_owned(), desired_entry());
        expected
    }
}

fn desired_entry() -> Value {
    json!({"command": "bkpr", "args": ["relay-mcp"], "disabled": false})
}

fn read_config(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!(
                "refusing to change {}: MCP configuration must be a regular file, not a symlink or other special file",
                path.display()
            )
        }
        Ok(_) => fs::read(path)
            .map(Some)
            .with_context(|| format!("read AGY MCP configuration {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("inspect AGY MCP configuration {}", path.display()))
        }
    }
}

fn install_agy(
    home: &Path,
    search_path: &OsStr,
    cwd: &Path,
    dry_run: bool,
    resolve_agy: impl FnOnce() -> Result<PathBuf>,
) -> Result<Outcome> {
    let config = Config::inspect(home.join(".gemini/config/mcp_config.json"))?;
    if config.already_configured()? {
        return Ok(Outcome::AlreadyConfigured(config.path));
    }
    require_installed_bkpr(search_path, cwd)?;
    if dry_run {
        return Ok(Outcome::DryRun(config.path));
    }
    let executable = resolve_agy().context("install AGY before running bkpr install agy")?;
    // Native add overwrites conflicts. Recheck the inspected bytes immediately before invoking it.
    if read_config(&config.path)? != config.bytes {
        bail!(
            "{} changed during installation; no MCP add was invoked; inspect it and retry",
            config.path.display()
        );
    }
    let output = Command::new(&executable)
        .args(["mcp", "add", "bkpr", "bkpr", "relay-mcp"])
        .env("HOME", home)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("execute {} mcp add", executable.display()))?;
    if !output.status.success() {
        bail!(
            "AGY MCP add failed ({}): {}. Inspect {} before retrying; the native command may have changed it",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            config.path.display()
        );
    }
    let actual = Config::inspect(config.path.clone())?;
    if actual.value != config.expected_after_add() {
        bail!(
            "AGY MCP add did not produce the expected configuration while preserving existing settings in {}; inspect the file before retrying",
            config.path.display()
        );
    }
    Ok(Outcome::Configured(config.path))
}

fn require_installed_bkpr(search_path: &OsStr, cwd: &Path) -> Result<()> {
    for directory in env::split_paths(search_path) {
        let candidate = if directory.is_absolute() {
            directory.join("bkpr")
        } else {
            cwd.join(&directory).join("bkpr")
        };
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            continue;
        }
        let resolved = fs::canonicalize(&candidate)
            .with_context(|| format!("resolve installed bkpr {}", candidate.display()))?;
        if !directory.is_absolute() || is_cargo_build(&candidate) || is_cargo_build(&resolved) {
            bail!(
                "bkpr on PATH ({}) is not a stable installation; install bkpr in an absolute PATH directory before running bkpr install agy (development target binaries and relative PATH entries are not persisted)",
                candidate.display()
            );
        }
        // Persist the name, not a development executable or immutable Nix store target.
        return Ok(());
    }
    bail!("bkpr is not installed on PATH; install bkpr before running bkpr install agy")
}

fn is_cargo_build(path: &Path) -> bool {
    let mut in_target = false;
    for component in path.components() {
        if let Component::Normal(name) = component {
            if in_target && (name == "debug" || name == "release") {
                return true;
            }
            in_target |= name == "target";
        }
    }
    false
}

// serde_json::Value otherwise accepts duplicate object keys using last-write-wins semantics.
// Refuse them rather than letting inspection and the native parser disagree about a conflict.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
                Ok(value.into())
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
                Ok(value.into())
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(Value::Array(values))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
                    match values.entry(key) {
                        serde_json::map::Entry::Vacant(entry) => {
                            entry.insert(value.0);
                        }
                        serde_json::map::Entry::Occupied(entry) => {
                            return Err(de::Error::custom(format!(
                                "duplicate JSON key {:?}",
                                entry.key()
                            )));
                        }
                    }
                }
                Ok(Value::Object(values))
            }
        }
        deserializer.deserialize_any(UniqueVisitor).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        root: tempfile::TempDir,
        home: PathBuf,
        bin: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let bin = root.path().join("bin");
            fs::create_dir(&home).unwrap();
            fs::create_dir(&bin).unwrap();
            executable(&bin.join("bkpr"), "#!/bin/sh\nexit 99\n");
            Self { root, home, bin }
        }

        fn config_path(&self) -> PathBuf {
            self.home.join(".gemini/config/mcp_config.json")
        }

        fn write_config(&self, bytes: impl AsRef<[u8]>) {
            let path = self.config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn install(&self, dry_run: bool) -> Result<Outcome> {
            install_agy(
                &self.home,
                self.bin.as_os_str(),
                self.root.path(),
                dry_run,
                || Ok(self.bin.join("agy")),
            )
        }

        fn native_add(&self, result: &Value) {
            // A private native-command fixture: check the actual argv and HOME passed by apply.
            let json = serde_json::to_string_pretty(result).unwrap();
            executable(
                &self.bin.join("agy"),
                &format!(
                    "#!/bin/sh\n[ \"$#\" = 5 ] && [ \"$1\" = mcp ] && [ \"$2\" = add ] && [ \"$3\" = bkpr ] && [ \"$4\" = bkpr ] && [ \"$5\" = relay-mcp ] || exit 41\n[ \"$HOME\" = '{}' ] || exit 42\nmkdir -p \"$HOME/.gemini/config\" || exit 43\ncat > \"$HOME/.gemini/config/mcp_config.json\" <<'BKPR_CONFIG'\n{json}\nBKPR_CONFIG\n",
                    self.home.display()
                ),
            );
        }
    }

    fn executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn equivalent_enabled_configuration_is_a_byte_preserving_noop() {
        for entry in [
            desired_entry(),
            json!({"command": "bkpr", "args": ["relay-mcp"]}),
        ] {
            let fixture = Fixture::new();
            let bytes =
                serde_json::to_vec_pretty(&json!({"mcpServers": {"bkpr": entry}, "keep": true}))
                    .unwrap();
            fixture.write_config(&bytes);
            // No native executable is present; an accidental invocation would fail.
            assert_eq!(
                fixture.install(false).unwrap(),
                Outcome::AlreadyConfigured(fixture.config_path())
            );
            assert_eq!(fs::read(fixture.config_path()).unwrap(), bytes);
        }
    }

    #[test]
    fn conflicts_disabled_entries_and_extra_settings_are_not_overwritten() {
        for entry in [
            json!({"command": "other", "args": ["relay-mcp"]}),
            json!({"command": "/work/target/debug/bkpr", "args": ["relay-mcp"]}),
            json!({"command": "/nix/store/old-bkpr/bin/bkpr", "args": ["relay-mcp"]}),
            json!({"command": "bkpr", "args": ["relay-mcp"], "disabled": true}),
            json!({"command": "bkpr", "args": ["relay-mcp"], "env": {"TOKEN": "private"}}),
            json!({"command": "bkpr", "args": ["relay-mcp"], "future": true}),
            Value::Null,
        ] {
            let fixture = Fixture::new();
            let bytes = serde_json::to_vec(&json!({"mcpServers": {"bkpr": entry}})).unwrap();
            fixture.write_config(&bytes);
            for dry_run in [false, true] {
                let error = fixture.install(dry_run).unwrap_err().to_string();
                assert!(error.contains("conflicting or disabled"), "{error}");
                assert_eq!(fs::read(fixture.config_path()).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn dry_run_neither_creates_directories_nor_invokes_native_add() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.install(true).unwrap(),
            Outcome::DryRun(fixture.config_path())
        );
        assert!(!fixture.home.join(".gemini").exists());
        let bytes = b"{ \"keep\": true }\n";
        fixture.write_config(bytes);
        assert_eq!(
            fixture.install(true).unwrap(),
            Outcome::DryRun(fixture.config_path())
        );
        assert_eq!(fs::read(fixture.config_path()).unwrap(), bytes);
    }

    #[test]
    fn malformed_ambiguous_and_jsonc_configuration_is_unchanged() {
        for bytes in [
            "{",
            "[]",
            "null",
            "{\"mcpServers\":null}",
            "{\"mcpServers\":[]}",
            "{\"mcpServers\":{},\"mcpServers\":{}}",
            "{\"nested\":{\"x\":1,\"x\":2}}",
            "{ // comment\n}",
            "{\"keep\":true,}",
        ] {
            let fixture = Fixture::new();
            fixture.write_config(bytes);
            assert!(fixture.install(false).is_err(), "accepted {bytes}");
            assert_eq!(fs::read(fixture.config_path()).unwrap(), bytes.as_bytes());
        }
    }

    #[test]
    fn symlink_configuration_is_never_followed_for_writing() {
        let fixture = Fixture::new();
        let original = fixture.root.path().join("original.json");
        fs::write(&original, "{}").unwrap();
        fs::create_dir_all(fixture.config_path().parent().unwrap()).unwrap();
        symlink(&original, fixture.config_path()).unwrap();
        assert!(
            fixture
                .install(false)
                .unwrap_err()
                .to_string()
                .contains("regular file")
        );
        assert_eq!(fs::read_to_string(original).unwrap(), "{}");
    }

    #[test]
    fn requires_a_stable_installed_command_without_persisting_nix_store_paths() {
        let fixture = Fixture::new();
        assert!(require_installed_bkpr(fixture.bin.as_os_str(), fixture.root.path()).is_ok());
        let build = fixture.root.path().join("target/debug");
        fs::create_dir_all(&build).unwrap();
        executable(&build.join("bkpr"), "#!/bin/sh\nexit 99\n");
        assert!(require_installed_bkpr(build.as_os_str(), fixture.root.path()).is_err());
        fs::remove_file(fixture.bin.join("bkpr")).unwrap();
        symlink(build.join("bkpr"), fixture.bin.join("bkpr")).unwrap();
        assert!(
            fixture
                .install(true)
                .unwrap_err()
                .to_string()
                .contains("stable installation")
        );
        assert!(require_installed_bkpr(OsStr::new("bin"), fixture.root.path()).is_err());
        assert!(require_installed_bkpr(OsStr::new("/not/installed"), fixture.root.path()).is_err());
        assert!(is_cargo_build(Path::new(
            "/work/target/aarch64-linux/debug/bkpr"
        )));
        assert!(!is_cargo_build(Path::new(
            "/nix/store/immutable-bkpr/bin/bkpr"
        )));
    }

    #[test]
    fn concurrent_change_before_native_add_is_not_overwritten() {
        let fixture = Fixture::new();
        fixture.write_config("{}");
        let result = install_agy(
            &fixture.home,
            fixture.bin.as_os_str(),
            fixture.root.path(),
            false,
            || {
                fixture.write_config("{\"changed\":true}");
                Ok(fixture.bin.join("agy"))
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changed during installation")
        );
        assert_eq!(
            fs::read_to_string(fixture.config_path()).unwrap(),
            "{\"changed\":true}"
        );
    }

    #[test]
    fn native_failure_and_unexpected_changes_are_reported_without_false_success() {
        let fixture = Fixture::new();
        fixture.write_config("{}");
        executable(
            &fixture.bin.join("agy"),
            "#!/bin/sh\nprintf 'native failure' >&2\nexit 9\n",
        );
        let error = fixture.install(false).unwrap_err().to_string();
        assert!(error.contains("native failure"), "{error}");
        assert_eq!(fs::read_to_string(fixture.config_path()).unwrap(), "{}");
        fixture.write_config("{\"keep\":true}");
        fixture.native_add(&json!({"mcpServers": {"bkpr": desired_entry()}}));
        assert!(
            fixture
                .install(false)
                .unwrap_err()
                .to_string()
                .contains("preserving existing settings")
        );
    }
}
