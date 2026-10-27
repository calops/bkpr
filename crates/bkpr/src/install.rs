//! Explicit, opt-in persistent harness configuration. No launch credentials belong here.
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Install persistent user integration for this harness.
    #[arg(value_enum)]
    target: Target,
    /// Select an Omp profile (otherwise respects OMP_PROFILE / PI_PROFILE).
    #[arg(long)]
    profile: Option<String>,
    /// Preflight and describe all changes without writing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum Target {
    Claude,
    Omp,
    #[value(alias = "antigravity")]
    Agy,
}

impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Omp => "omp",
            Self::Agy => "agy",
        }
    }
}

pub(crate) fn run(args: Args) -> Result<()> {
    let home = absolute_home()?;
    if args.profile.is_some() && args.target != Target::Omp {
        bail!("--profile is only supported by bkpr install omp");
    }
    let program = installed_program(&env::var_os("PATH").unwrap_or_default())?;
    let locations = Locations::resolve(args.target, &home, args.profile.as_deref())?;
    let plan = plan(args.target, &locations, &program)?;
    let changed = plan.files.iter().filter(|file| file.changed()).count();
    if !args.dry_run {
        plan.apply()?;
    }
    println!(
        "{} {} integration in {} ({} changed assets). Only public commands and integration assets are persisted; runtime readiness is established by the next managed interactive launch, not this configuration.",
        if args.dry_run {
            "Would configure"
        } else if changed == 0 {
            "Already configured"
        } else {
            "Configured"
        },
        args.target.name(),
        locations.mcp.display(),
        changed
    );
    Ok(())
}

fn absolute_home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context("install requires an absolute HOME")
}

fn absolute_env(name: &str, default: PathBuf) -> Result<PathBuf> {
    let path = env::var_os(name)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default);
    if !path.is_absolute() {
        bail!("{name} must be absolute for persistent installation");
    }
    Ok(path)
}

struct Locations {
    mcp: PathBuf,
    assets: PathBuf,
    settings: Option<PathBuf>,
}
impl Locations {
    fn resolve(target: Target, home: &Path, profile: Option<&str>) -> Result<Self> {
        Ok(match target {
            Target::Agy => Self {
                mcp: home.join(".gemini/config/mcp_config.json"),
                assets: home.join(".gemini"),
                settings: None,
            },
            Target::Claude => {
                let assets = absolute_env("CLAUDE_CONFIG_DIR", home.join(".claude"))?;
                // Claude's custom config directory relocates its global state file too.
                let mcp = if env::var_os("CLAUDE_CONFIG_DIR").is_some_and(|s| !s.is_empty()) {
                    assets.join(".claude.json")
                } else {
                    home.join(".claude.json")
                };
                Self {
                    mcp,
                    settings: Some(assets.join("settings.json")),
                    assets,
                }
            }
            Target::Omp => {
                let selected = profile
                    .map(str::to_owned)
                    .or_else(|| env::var("OMP_PROFILE").ok())
                    .or_else(|| env::var("PI_PROFILE").ok())
                    .unwrap_or_default();
                let root = match env::var_os("PI_CONFIG_DIR").filter(|s| !s.is_empty()) {
                    Some(value) => {
                        let value = PathBuf::from(value);
                        if value.is_absolute() {
                            value
                        } else {
                            home.join(value)
                        }
                    }
                    None => home.join(".omp"),
                };
                let mut override_dir = env::var_os("PI_CODING_AGENT_DIR")
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from);
                for inherited in [env::var("OMP_PROFILE").ok(), env::var("PI_PROFILE").ok()]
                    .into_iter()
                    .flatten()
                {
                    if !inherited.trim().is_empty()
                        && inherited.trim() != "default"
                        && override_dir.as_ref()
                            == Some(&root.join("profiles").join(inherited.trim()).join("agent"))
                    {
                        override_dir = None;
                    }
                }
                let assets = omp_agent_dir(&root, override_dir.as_deref(), &selected)?;
                Self {
                    mcp: assets.join("mcp.json"),
                    settings: None,
                    assets,
                }
            }
        })
    }
}

fn omp_agent_dir(root: &Path, override_dir: Option<&Path>, profile: &str) -> Result<PathBuf> {
    let profile = profile.trim();
    if profile.is_empty() || profile == "default" {
        let path = override_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("agent"));
        if !path.is_absolute() {
            bail!("PI_CODING_AGENT_DIR must be absolute for persistent installation");
        }
        return Ok(path);
    }
    let basename = profile.split('.').next().unwrap();
    let reserved = matches!(
        basename.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
    ) || basename.len() == 4
        && (basename.starts_with("com") || basename.starts_with("lpt"))
        && basename.as_bytes()[3].is_ascii_digit();
    if profile.len() > 64
        || !profile.as_bytes()[0].is_ascii_lowercase() && !profile.as_bytes()[0].is_ascii_digit()
        || profile.ends_with('.')
        || reserved
        || !profile.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        bail!("invalid Omp profile name {profile:?}");
    }
    Ok(root.join("profiles").join(profile).join("agent"))
}

fn installed_program(search_path: &OsStr) -> Result<PathBuf> {
    for directory in env::split_paths(search_path) {
        let candidate = directory.join("bkpr");
        // Session wrappers may precede a real installation on PATH.
        if validate_public_program(&candidate).is_err() {
            continue;
        }
        return Ok(candidate);
    }
    bail!(
        "bkpr requires a stable absolute public installation/profile path on PATH; development, session wrapper and immutable Nix store paths cannot be persisted"
    )
}

/// Validate a public executable without replacing its upgrade-stable spelling
/// with a canonical immutable package path. Runtime callers must supply argv[0],
/// not rediscover another executable through PATH.
pub(crate) fn validate_public_program(path: &Path) -> Result<()> {
    path.to_str()
        .context("public bkpr installation path must be UTF-8")?;
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || is_cargo_build(path)
        || path.starts_with("/nix/store")
        || path.starts_with("/run/user")
    {
        bail!("bkpr command must be a stable absolute public installation/profile path");
    }
    let metadata = fs::metadata(path)
        .with_context(|| format!("inspect public bkpr command {}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!("public bkpr command is not an executable regular file");
    }
    let resolved = fs::canonicalize(path)?;
    if is_cargo_build(&resolved) {
        bail!("public bkpr command points to a development build");
    }
    Ok(())
}

fn is_cargo_build(path: &Path) -> bool {
    let mut target = false;
    for component in path.components() {
        if let Component::Normal(name) = component {
            if target && (name == "debug" || name == "release") {
                return true;
            }
            target |= name == "target";
        }
    }
    false
}

// Refuse symlinks in every ancestor, including missing-file parents. HOME itself
// and harness config roots must not redirect an opt-in write to another tree.
fn safe_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!(
            "installation path must be absolute and contain no parent traversal: {}",
            path.display()
        );
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => bail!(
                "refusing symlink installation artifact {}",
                current.display()
            ),
            Ok(metadata) if current != path && !metadata.is_dir() => bail!(
                "installation ancestor is not a directory: {}",
                current.display()
            ),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_asset(path: &Path) -> Result<Option<Vec<u8>>> {
    safe_path(path)?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > 16 * 1024 * 1024
    {
        bail!(
            "refusing unsafe or oversized installation artifact {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

struct Config {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    value: Value,
}
impl Config {
    fn inspect(path: PathBuf) -> Result<Self> {
        let bytes = read_asset(&path)?;
        let value = match &bytes {
            Some(bytes) => {
                serde_json::from_slice::<UniqueValue>(bytes)
                    .with_context(|| {
                        format!("refusing malformed or ambiguous JSON in {}", path.display())
                    })?
                    .0
            }
            None => json!({}),
        };
        if !value.is_object() {
            bail!("expected JSON object in {}", path.display());
        }
        if value
            .get("mcpServers")
            .is_some_and(|servers| !servers.is_object())
        {
            bail!("mcpServers must be an object in {}", path.display());
        }
        Ok(Self { path, bytes, value })
    }
    fn set_server(&mut self, target: Target, program: &Path) -> Result<()> {
        if self
            .value
            .get("disabledServers")
            .is_some_and(|v| v.as_array().is_none_or(|v| v.iter().any(|s| s == "bkpr")))
        {
            bail!(
                "bkpr is disabled or disabledServers is malformed in {}",
                self.path.display()
            );
        }
        let desired = server_entry(target, program);
        let servers = self
            .value
            .as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .with_context(|| format!("mcpServers must be an object in {}", self.path.display()))?;
        if let Some(existing) = servers.get("bkpr") {
            // LEGACY_COMPAT(next-major): migrate the original MCP-only AGY
            // registration during explicit installation, never at launch.
            let legacy = target == Target::Agy
                && (existing == &json!({"command":"bkpr","args":["relay-mcp"],"disabled":false})
                    || existing == &json!({"command":"bkpr","args":["relay-mcp"]}));
            if existing != &desired && !legacy {
                bail!(
                    "refusing conflicting or disabled bkpr MCP entry in {}",
                    self.path.display()
                );
            }
        }
        servers.insert("bkpr".into(), desired);
        Ok(())
    }
    fn into_file(self) -> Result<PlannedFile> {
        let unchanged = self.bytes.as_ref().is_some_and(|bytes| {
            serde_json::from_slice::<Value>(bytes).ok().as_ref() == Some(&self.value)
        });
        let wanted = if unchanged {
            self.bytes.clone().unwrap()
        } else {
            let mut bytes = serde_json::to_vec_pretty(&self.value)?;
            bytes.push(b'\n');
            bytes
        };
        Ok(PlannedFile {
            path: self.path,
            before: self.bytes,
            wanted,
        })
    }
}

fn server_entry(target: Target, program: &Path) -> Value {
    if target == Target::Agy {
        json!({"command":program,"args":["integration",target.name(),"mcp"],"disabled":false})
    } else {
        json!({"type":"stdio","command":program,"args":["integration",target.name(),"mcp"]})
    }
}

struct PlannedFile {
    path: PathBuf,
    before: Option<Vec<u8>>,
    wanted: Vec<u8>,
}
impl PlannedFile {
    fn asset(path: PathBuf, wanted: Vec<u8>, owned_hash: Option<&str>) -> Result<Self> {
        let before = read_asset(&path)?;
        if before
            .as_ref()
            .is_some_and(|bytes| *bytes != wanted && owned_hash != Some(asset_hash(bytes).as_str()))
        {
            bail!(
                "refusing modified or unowned installed asset {}",
                path.display()
            );
        }
        Ok(Self {
            path,
            before,
            wanted,
        })
    }
    fn changed(&self) -> bool {
        self.before.as_ref() != Some(&self.wanted)
    }
}
struct Plan {
    files: Vec<PlannedFile>,
}
impl Plan {
    fn apply(self) -> Result<()> {
        // Preflight ALL owned files before the first write, and revalidate snapshots.
        for file in &self.files {
            if read_asset(&file.path)? != file.before {
                bail!(
                    "{} changed during installation; no changes applied",
                    file.path.display()
                );
            }
        }
        for file in self.files.into_iter().filter(PlannedFile::changed) {
            let parent = file.path.parent().context("installation file parent")?;
            fs::create_dir_all(parent)?;
            safe_path(&file.path)?;
            if read_asset(&file.path)? != file.before {
                bail!("{} changed during installation", file.path.display());
            }
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            temporary.write_all(&file.wanted)?;
            temporary.as_file().sync_all()?;
            if file.before.is_none() {
                temporary
                    .persist_noclobber(&file.path)
                    .map_err(|e| e.error)?;
            } else {
                temporary.persist(&file.path).map_err(|e| e.error)?;
            }
        }
        Ok(())
    }
}

const SKILL_ASSET: &str = "skills/bkpr-control/SKILL.md";
const REFERENCE_ASSET: &str = "skills/bkpr-control/references/control.md";
const EXTENSION_ASSET: &str = "extensions/bkpr.ts";

fn asset_hash(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut hash, "{byte:02x}").expect("writing a digest into a String");
    }
    hash
}

fn asset_names(target: Target) -> &'static [&'static str] {
    match target {
        Target::Claude => &[SKILL_ASSET, REFERENCE_ASSET],
        Target::Omp => &[EXTENSION_ASSET, SKILL_ASSET, REFERENCE_ASSET],
        Target::Agy => &[],
    }
}

fn installation_manifest(target: Target, paths: &Locations, program: &Path) -> Result<Config> {
    let manifest = Config::inspect(paths.assets.join(".bkpr-install.json"))?;
    if manifest.bytes.is_some() {
        let object = manifest.value.as_object().unwrap();
        if object.len() != 4
            || object.get("format") != Some(&json!(1))
            || object.get("harness") != Some(&json!(target.name()))
            || object.get("program") != Some(&json!(program))
        {
            bail!(
                "conflicting Bkpr installation manifest {}",
                manifest.path.display()
            );
        }
        let hashes = object
            .get("assets")
            .and_then(Value::as_object)
            .context("invalid installed asset hashes")?;
        if hashes.len() != asset_names(target).len()
            || asset_names(target).iter().any(|name| {
                hashes
                    .get(*name)
                    .and_then(Value::as_str)
                    .is_none_or(|hash| {
                        hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                    })
            })
        {
            bail!("invalid Bkpr installation manifest asset set");
        }
    }
    Ok(manifest)
}

fn plan(target: Target, paths: &Locations, program: &Path) -> Result<Plan> {
    let mut manifest = installation_manifest(target, paths, program)?;
    let mut hashes = Map::new();
    let mut config = Config::inspect(paths.mcp.clone())?;
    config.set_server(target, program)?;
    let mut files = vec![config.into_file()?];
    if target == Target::Claude {
        let mut settings = Config::inspect(paths.settings.clone().unwrap())?;
        if settings
            .value
            .get("disableAllHooks")
            .is_some_and(|v| v != false)
        {
            bail!(
                "Claude hooks are explicitly disabled in {}",
                settings.path.display()
            );
        }
        let wanted = claude_hooks(program)?;
        merge_hooks(&mut settings.value, &wanted)?;
        files.push(settings.into_file()?);
    }
    if target == Target::Omp {
        require_omp_enabled(&paths.assets, None)?;
        add_asset(
            &mut files,
            &mut hashes,
            &manifest,
            &paths.assets,
            EXTENSION_ASSET,
            crate::omp::installed_extension(program)?.into_bytes(),
        )?;
    }
    if target != Target::Agy {
        add_asset(
            &mut files,
            &mut hashes,
            &manifest,
            &paths.assets,
            SKILL_ASSET,
            crate::relay::CONTROL_SKILL.as_bytes().to_vec(),
        )?;
        add_asset(
            &mut files,
            &mut hashes,
            &manifest,
            &paths.assets,
            REFERENCE_ASSET,
            crate::relay::CONTROL_REFERENCE.as_bytes().to_vec(),
        )?;
    }
    manifest.value = json!({"format":1,"harness":target.name(),"program":program,"assets":hashes});
    files.push(manifest.into_file()?);
    Ok(Plan { files })
}

fn add_asset(
    files: &mut Vec<PlannedFile>,
    hashes: &mut Map<String, Value>,
    manifest: &Config,
    root: &Path,
    name: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    let old_hash = manifest
        .value
        .get("assets")
        .and_then(|assets| assets.get(name))
        .and_then(Value::as_str);
    hashes.insert(name.into(), asset_hash(&bytes).into());
    files.push(PlannedFile::asset(root.join(name), bytes, old_hash)?);
    Ok(())
}

fn claude_hooks(program: &Path) -> Result<Value> {
    let quoted = crate::claude::shell_quote(
        program
            .to_str()
            .context("bkpr installation path is not UTF-8")?,
    );
    let mut settings = json!({});
    crate::claude::compose_installed(&mut settings, &quoted)?;
    Ok(settings.as_object_mut().unwrap().remove("hooks").unwrap())
}

fn merge_hooks(settings: &mut Value, wanted: &Value) -> Result<()> {
    let hooks = settings
        .as_object_mut()
        .context("Claude settings must be an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude hooks must be an object")?;
    for (event, callbacks) in wanted.as_object().unwrap() {
        let existing = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("Claude {event} hooks must be an array"))?;
        for callback in callbacks.as_array().unwrap() {
            let matches = existing.iter().filter(|value| *value == callback).count();
            if matches > 1 {
                bail!("refusing duplicate Bkpr Claude {event} hooks");
            }
            if matches == 1 {
                continue;
            }
            // Do not stack new owned hooks over a modified, old, or disabled Bkpr hook.
            if existing.iter().any(|value| {
                value.to_string().contains("bkpr")
                    && value
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                let command =
                                    hook.get("command").and_then(Value::as_str).unwrap_or("");
                                let wanted_command =
                                    callback["hooks"][0]["command"].as_str().unwrap();
                                let role = if wanted_command.contains(" claude-wait") {
                                    "claude-wait"
                                } else if wanted_command.contains(" claude-inbox") {
                                    "claude-inbox"
                                } else {
                                    "claude-title"
                                };
                                command.split_whitespace().any(|part| part == role)
                            })
                        })
            }) {
                bail!("refusing conflicting Bkpr Claude {event} hook");
            }
            existing.push(callback.clone());
        }
    }
    Ok(())
}

fn require_omp_enabled(agent: &Path, cwd: Option<&Path>) -> Result<()> {
    let mut bases = vec![agent.to_path_buf()];
    if let Some(cwd) = cwd {
        bases.push(cwd.join(".omp"));
    }
    for base in bases {
        for filename in ["config.yml", "config.yaml", "settings.json"] {
            if let Some(bytes) = read_asset(&base.join(filename))? {
                let settings = if filename.ends_with("json") {
                    serde_json::from_slice::<UniqueValue>(&bytes)?.0
                } else {
                    serde_yaml::from_slice::<UniqueValue>(&bytes)
                        .context("parse unambiguous Omp YAML settings")?
                        .0
                };
                check_omp_settings(&settings)?;
            }
        }
    }
    Ok(())
}

fn check_omp_settings(settings: &Value) -> Result<()> {
    if !settings.is_object() {
        bail!("Omp settings must be a mapping");
    }
    if settings.get("<<").is_some() {
        bail!("Omp YAML merge keys require explicit review before installing Bkpr");
    }
    if let Some(disabled) = settings.get("disabledExtensions") {
        let disabled = disabled
            .as_array()
            .context("disabledExtensions must be an array")?;
        if disabled.iter().any(|v| {
            matches!(
                v.as_str(),
                Some("extension-module:bkpr" | "skill:bkpr-control")
            )
        }) {
            bail!("Bkpr extension or skill is explicitly disabled in Omp settings");
        }
    }
    Ok(())
}

/// A configured installed server must never be overlaid with a second identity.
/// Complete configurations use their installed assets. Explicit restrictions are
/// left to the harness, while incomplete configurations fail rather than being
/// silently repaired by launch-local injection.
pub(crate) fn installed_launch(harness: &str, arguments: &[OsString]) -> Result<bool> {
    let target = match harness {
        "claude" => Target::Claude,
        "omp" => Target::Omp,
        "agy" => Target::Agy,
        _ => return Ok(false),
    };
    // Administrative/noninteractive routes do not need HOME, installation files,
    // or a broker identity. Let the real harness interpret them unchanged.
    let interactive = match target {
        Target::Claude => crate::claude::interactive_arguments(arguments),
        Target::Omp => crate::omp::interactive_arguments(arguments),
        Target::Agy => crate::agy::interactive_arguments(arguments),
    };
    if !interactive {
        return Ok(true);
    }
    let home = absolute_home()?;
    let profile = option_value(arguments, "--profile");
    let paths = Locations::resolve(target, &home, profile.as_deref())?;
    installed_launch_at(
        target,
        &paths,
        arguments,
        env::current_dir().ok().as_deref(),
    )
}

fn installed_launch_at(
    target: Target,
    paths: &Locations,
    arguments: &[OsString],
    cwd: Option<&Path>,
) -> Result<bool> {
    let config = Config::inspect(paths.mcp.clone())?;
    let Some(entry) = config
        .value
        .get("mcpServers")
        .and_then(|servers| servers.get("bkpr"))
    else {
        return Ok(false);
    };
    let Some(program) = entry
        .get("command")
        .and_then(Value::as_str)
        .map(PathBuf::from)
    else {
        return Ok(false);
    };
    if entry.get("args") != Some(&json!(["integration", target.name(), "mcp"])) {
        return Ok(false);
    }
    if !program.is_absolute() || is_cargo_build(&program) || program.starts_with("/nix/store") {
        bail!("installed Bkpr command is not a stable public path");
    }
    // Restrictions intentionally keep the installed MCP/hooks/extension disabled;
    // never add an explicit CLI package/config to evade the user's selection.
    if launch_restricted(
        arguments,
        &[
            "--bare",
            "--strict-mcp-config",
            "--setting-sources",
            "--settings",
            "--no-extensions",
            "--trusted-extension",
            "--no-skills",
            "--config",
        ],
    ) {
        return Ok(true);
    }
    validate_installed(target, paths, &program)?;
    if target == Target::Omp {
        require_omp_enabled(&paths.assets, cwd)?;
    }
    Ok(true)
}

fn validate_installed(target: Target, paths: &Locations, program: &Path) -> Result<()> {
    let mut config = Config::inspect(paths.mcp.clone())?;
    config.set_server(target, program)?;
    let manifest = installation_manifest(target, paths, program)?;
    if manifest.bytes.is_none() {
        bail!(
            "installed Bkpr manifest is missing; rerun bkpr install {}",
            target.name()
        );
    }
    for name in asset_names(target) {
        let bytes = read_asset(&paths.assets.join(name))?
            .with_context(|| format!("installed Bkpr asset {name} is missing"))?;
        if manifest.value["assets"][*name].as_str() != Some(asset_hash(&bytes).as_str()) {
            bail!("installed Bkpr asset {name} has been modified");
        }
    }
    if target == Target::Claude {
        let mut settings = Config::inspect(paths.settings.clone().unwrap())?;
        if settings
            .value
            .get("disableAllHooks")
            .is_some_and(|v| v != false)
        {
            bail!("Claude hooks are explicitly disabled");
        }
        let before = settings.value.clone();
        merge_hooks(&mut settings.value, &claude_hooks(program)?)?;
        if before != settings.value {
            bail!("installed Bkpr hooks are incomplete; rerun bkpr install claude");
        }
    }
    Ok(())
}

pub(crate) fn launch_restricted(arguments: &[OsString], flags: &[&str]) -> bool {
    options(arguments).any(|(option, _)| flags.contains(&option))
}

fn option_value(arguments: &[OsString], flag: &str) -> Option<String> {
    options(arguments)
        .filter_map(|(option, value)| (option == flag).then_some(value))
        .last()
        .flatten()
        .map(str::to_owned)
}

// Consume values before interpreting the next argument as a switch. In
// particular, a model/prompt/config value may itself start with "--".
fn options(arguments: &[OsString]) -> impl Iterator<Item = (&str, Option<&str>)> {
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < arguments.len() {
            let argument = arguments[index].to_str()?;
            index += 1;
            if argument == "--" {
                index = arguments.len();
                return None;
            }
            if !argument.starts_with('-') {
                continue;
            }
            let (option, inline) = argument
                .split_once('=')
                .map_or((argument, None), |(option, value)| (option, Some(value)));
            let required = matches!(
                option,
                "--model"
                    | "--name"
                    | "-n"
                    | "--settings"
                    | "--setting-sources"
                    | "--permission-mode"
                    | "--session-id"
                    | "--effort"
                    | "--plugin-dir"
                    | "--cwd"
                    | "--add-dir"
                    | "--profile"
                    | "--config"
                    | "--session-dir"
                    | "--fork"
                    | "--smol"
                    | "--slow"
                    | "--plan"
                    | "--models"
                    | "--provider"
                    | "--api-key"
                    | "--provider-session-id"
                    | "--prompt-cache-key"
                    | "--service-tier"
                    | "--thinking"
                    | "--prewalk-into"
                    | "--plan-yolo-into"
                    | "--goal"
                    | "--tools"
                    | "--approval-mode"
                    | "--max-time"
                    | "--extension"
                    | "-e"
                    | "--hook"
                    | "--skills"
                    | "--system-prompt"
                    | "--system-prompt-template"
                    | "--append-system-prompt"
                    | "--mode"
                    | "--export"
                    | "--alias"
                    | "--trusted-extension"
                    | "--output-format"
                    | "--input-format"
                    | "--allowedTools"
                    | "--disallowedTools"
                    | "--agent"
                    | "--conversation"
                    | "--prompt-interactive"
                    | "-i"
                    | "--project"
                    | "--log-file"
            );
            let variadic = matches!(
                option,
                "--mcp-config" | "--channels" | "--dangerously-load-development-channels"
            );
            let optional = matches!(option, "--resume" | "-r" | "--session" | "--restore");
            let value = if inline.is_some() {
                inline
            } else if required || variadic {
                let value = arguments.get(index).and_then(|value| value.to_str());
                index = (index + 1).min(arguments.len());
                value
            } else if optional
                && arguments
                    .get(index)
                    .is_some_and(|next| !next.as_encoded_bytes().starts_with(b"-"))
            {
                let value = arguments[index].to_str();
                index += 1;
                value
            } else {
                None
            };
            if variadic {
                while index < arguments.len()
                    && !arguments[index].as_encoded_bytes().starts_with(b"-")
                {
                    index += 1;
                }
            }
            return Some((option, value));
        }
        None
    })
}

// serde_json::Value silently accepts duplicate keys. Installation must not.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Value;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(value.into())
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
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(value.into())
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
                    if values.insert(key.clone(), value.0).is_some() {
                        return Err(de::Error::custom(format!("duplicate JSON key {key:?}")));
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

    fn locations(root: &Path, target: Target) -> Locations {
        let assets = fs::canonicalize(root).unwrap().join(target.name());
        Locations {
            mcp: assets.join("mcp.json"),
            settings: (target == Target::Claude).then(|| assets.join("settings.json")),
            assets,
        }
    }
    fn write(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    #[test]
    fn every_installer_preserves_unrelated_settings_and_is_byte_idempotent() {
        for target in [Target::Claude, Target::Omp, Target::Agy] {
            let root = tempfile::tempdir().unwrap();
            let paths = locations(root.path(), target);
            write(
                &paths.mcp,
                &json!({"mcpServers":{"other":{"command":"other"}},"keep":true}),
            );
            if let Some(settings) = &paths.settings {
                write(
                    settings,
                    &json!({"permissions":{"deny":["Bash(rm *)"]},"statusLine":{"command":"mine"},"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"mine"}]}]}}),
                );
            }
            let program = Path::new("/public/profile/bin/bkpr");
            plan(target, &paths, program).unwrap().apply().unwrap();
            let second = plan(target, &paths, program).unwrap();
            assert!(second.files.iter().all(|file| !file.changed()));
            assert_eq!(
                Config::inspect(paths.mcp.clone()).unwrap().value["keep"],
                true
            );
            if let Some(settings) = paths.settings {
                let value = Config::inspect(settings).unwrap().value;
                assert_eq!(value["permissions"]["deny"][0], "Bash(rm *)");
                assert_eq!(value["statusLine"]["command"], "mine");
                assert_eq!(
                    value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
                    "mine"
                );
            }
        }
    }
    #[test]
    fn dry_plan_creates_nothing_and_late_conflict_prevents_all_writes() {
        let root = tempfile::tempdir().unwrap();
        let paths = locations(root.path(), Target::Omp);
        let program = Path::new("/public/bin/bkpr");
        let pending = plan(Target::Omp, &paths, program).unwrap();
        assert!(!paths.assets.exists());
        write(&paths.mcp, &json!({"concurrent":true}));
        assert!(pending.apply().is_err());
        assert!(!paths.assets.join("extensions").exists());
        fs::create_dir_all(paths.assets.join("skills/bkpr-control")).unwrap();
        fs::write(paths.assets.join("skills/bkpr-control/SKILL.md"), "mine").unwrap();
        assert!(plan(Target::Omp, &paths, program).is_err());
        assert_eq!(
            Config::inspect(paths.mcp).unwrap().value,
            json!({"concurrent":true})
        );
    }
    // LEGACY_COMPAT(next-major): only the exact original owned entry may migrate.
    #[test]
    fn only_exact_legacy_agy_entry_is_migrated() {
        for entry in [
            json!({"command":"bkpr","args":["relay-mcp"]}),
            json!({"command":"bkpr","args":["relay-mcp"],"disabled":false}),
        ] {
            let root = tempfile::tempdir().unwrap();
            let paths = locations(root.path(), Target::Agy);
            write(&paths.mcp, &json!({"mcpServers":{"bkpr":entry}}));
            plan(Target::Agy, &paths, Path::new("/public/bin/bkpr"))
                .unwrap()
                .apply()
                .unwrap();
            assert_eq!(
                Config::inspect(paths.mcp).unwrap().value["mcpServers"]["bkpr"]["args"],
                json!(["integration", "agy", "mcp"])
            );
        }
    }
    #[test]
    fn disabled_conflicting_malformed_and_symlink_artifacts_are_refused() {
        for target in [Target::Claude, Target::Omp, Target::Agy] {
            for entry in [
                json!({"command":"other"}),
                json!({"command":"bkpr","args":["relay-mcp"],"disabled":true}),
                json!({"command":"bkpr","args":["relay-mcp"],"env":{"SECRET":"token"}}),
            ] {
                let root = tempfile::tempdir().unwrap();
                let paths = locations(root.path(), target);
                write(&paths.mcp, &json!({"mcpServers":{"bkpr":entry}}));
                let before = fs::read(&paths.mcp).unwrap();
                assert!(plan(target, &paths, Path::new("/public/bin/bkpr")).is_err());
                assert_eq!(fs::read(paths.mcp).unwrap(), before);
            }
        }
        let root = tempfile::tempdir().unwrap();
        let paths = locations(root.path(), Target::Omp);
        fs::create_dir_all(&paths.assets).unwrap();
        for malformed in ["[]", "{", "{\"x\":1,\"x\":2}", "{\"mcpServers\":null}"] {
            fs::write(&paths.mcp, malformed).unwrap();
            assert!(plan(Target::Omp, &paths, Path::new("/public/bin/bkpr")).is_err());
        }
        fs::remove_file(&paths.mcp).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "{}").unwrap();
        symlink(&outside, &paths.mcp).unwrap();
        assert!(plan(Target::Omp, &paths, Path::new("/public/bin/bkpr")).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "{}");
        let redirected = root.path().join("redirected");
        symlink(&paths.assets, &redirected).unwrap();
        assert!(read_asset(&redirected.join("new.json")).is_err());
    }
    #[test]
    fn native_disable_controls_are_not_reenabled() {
        let root = tempfile::tempdir().unwrap();
        let paths = locations(root.path(), Target::Omp);
        fs::create_dir_all(&paths.assets).unwrap();
        fs::write(
            paths.assets.join("config.yml"),
            "disabledExtensions:\n  - extension-module:bkpr\n",
        )
        .unwrap();
        assert!(plan(Target::Omp, &paths, Path::new("/public/bin/bkpr")).is_err());
        assert!(!paths.mcp.exists());
        let paths = locations(root.path(), Target::Claude);
        write(
            paths.settings.as_ref().unwrap(),
            &json!({"disableAllHooks":true}),
        );
        assert!(plan(Target::Claude, &paths, Path::new("/public/bin/bkpr")).is_err());
        assert!(!paths.mcp.exists());
    }
    #[test]
    fn profile_selection_isolated_and_validated_like_omp() {
        let root = Path::new("/home/test/.omp");
        let custom = Path::new("/custom/agent");
        assert_eq!(
            omp_agent_dir(root, Some(custom), "default").unwrap(),
            custom
        );
        assert_eq!(
            omp_agent_dir(root, Some(custom), " work ").unwrap(),
            root.join("profiles/work/agent")
        );
        for name in [
            "../escape",
            ".",
            "..",
            "bad/name",
            "UPPER",
            "con",
            "com1",
            "work.",
            "_start",
        ] {
            assert!(omp_agent_dir(root, None, name).is_err(), "{name}");
        }
        assert!(omp_agent_dir(root, Some(Path::new("relative")), "").is_err());
        let args = ["--profile=work", "--", "--profile=ignored"].map(OsString::from);
        assert_eq!(option_value(&args, "--profile").as_deref(), Some("work"));
    }

    #[test]
    fn complete_installed_assets_bypass_and_partial_assets_never_duplicate() {
        for target in [Target::Claude, Target::Omp, Target::Agy] {
            let root = tempfile::tempdir().unwrap();
            let paths = locations(root.path(), target);
            assert!(!installed_launch_at(target, &paths, &[], None).unwrap());
            plan(target, &paths, Path::new("/public/bin/bkpr"))
                .unwrap()
                .apply()
                .unwrap();
            assert!(installed_launch_at(target, &paths, &[], None).unwrap());
            if target != Target::Agy {
                fs::remove_file(paths.assets.join("skills/bkpr-control/SKILL.md")).unwrap();
                assert!(installed_launch_at(target, &paths, &[], None).is_err());
            }
            for flag in [
                "--bare",
                "--strict-mcp-config",
                "--no-extensions",
                "--trusted-extension=/approved/only.ts",
                "--config=custom.yml",
            ] {
                assert!(
                    installed_launch_at(target, &paths, &[flag.into()], None).unwrap(),
                    "{flag}"
                );
            }
        }
        assert!(!launch_restricted(
            &["--".into(), "--no-extensions".into()],
            &["--no-extensions"]
        ));
    }

    #[test]
    fn yaml_duplicates_and_explicit_mcp_denylist_do_not_enable_installation() {
        let root = tempfile::tempdir().unwrap();
        let paths = locations(root.path(), Target::Omp);
        fs::create_dir_all(&paths.assets).unwrap();
        fs::write(
            paths.assets.join("config.yml"),
            "disabledExtensions: [extension-module:bkpr]\ndisabledExtensions: []\n",
        )
        .unwrap();
        assert!(plan(Target::Omp, &paths, Path::new("/public/bin/bkpr")).is_err());
        fs::remove_file(paths.assets.join("config.yml")).unwrap();
        write(&paths.mcp, &json!({"disabledServers":["bkpr"]}));
        assert!(plan(Target::Omp, &paths, Path::new("/public/bin/bkpr")).is_err());
        assert_eq!(
            Config::inspect(paths.mcp).unwrap().value,
            json!({"disabledServers":["bkpr"]})
        );
    }

    #[test]
    fn public_command_keeps_profile_symlink_and_rejects_development_paths() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package/bin");
        let public = root.path().join("profile/bin");
        let build = root.path().join("target/debug");
        for directory in [&package, &public, &build] {
            fs::create_dir_all(directory).unwrap();
        }
        for directory in [&package, &build] {
            fs::write(directory.join("bkpr"), "#!/bin/sh\nexit 99\n").unwrap();
            fs::set_permissions(directory.join("bkpr"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        symlink(package.join("bkpr"), public.join("bkpr")).unwrap();
        assert_eq!(
            installed_program(public.as_os_str()).unwrap(),
            public.join("bkpr")
        );
        assert!(installed_program(build.as_os_str()).is_err());
        fs::remove_file(public.join("bkpr")).unwrap();
        symlink(build.join("bkpr"), public.join("bkpr")).unwrap();
        assert!(installed_program(public.as_os_str()).is_err());
        assert!(installed_program(OsStr::new("relative/bin")).is_err());
        assert!(is_cargo_build(Path::new(
            "/work/target/aarch64-linux/debug/bkpr"
        )));
    }

    #[test]
    fn option_values_cannot_become_profile_or_disable_switches() {
        let args = [
            "--model",
            "--no-skills",
            "--system-prompt",
            "--profile",
            "--profile",
            "work",
        ]
        .map(OsString::from);
        assert!(!launch_restricted(&args, &["--no-skills"]));
        assert_eq!(option_value(&args, "--profile").as_deref(), Some("work"));
        for (flag, value) in [
            ("--model", "--profile"),
            ("--settings", "--no-extensions"),
            ("--prompt-interactive", "--profile"),
            ("--mcp-config", "--no-skills"),
        ] {
            let args = [flag, value].map(OsString::from);
            assert!(!launch_restricted(
                &args,
                &["--no-skills", "--no-extensions"]
            ));
            assert_eq!(option_value(&args, "--profile"), None);
        }
        assert!(launch_restricted(
            &["--model=fast".into(), "--no-skills".into()],
            &["--no-skills"]
        ));
        for harness in ["claude", "omp", "agy"] {
            assert!(installed_launch(harness, &["--help".into()]).unwrap());
            assert!(installed_launch(harness, &["--version".into()]).unwrap());
        }
    }

    #[test]
    fn owned_old_assets_upgrade_but_user_edits_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let paths = locations(root.path(), Target::Omp);
        let program = Path::new("/public/bin/bkpr");
        plan(Target::Omp, &paths, program).unwrap().apply().unwrap();
        let skill = paths.assets.join(SKILL_ASSET);
        let old = b"previous bundled control skill\n";
        fs::write(&skill, old).unwrap();
        let manifest_path = paths.assets.join(".bkpr-install.json");
        let mut manifest = Config::inspect(manifest_path.clone()).unwrap().value;
        manifest["assets"][SKILL_ASSET] = asset_hash(old).into();
        write(&manifest_path, &manifest);
        // Normal launch accepts a valid installed older bundle.
        assert!(installed_launch_at(Target::Omp, &paths, &[], None).unwrap());
        let upgrade = plan(Target::Omp, &paths, program).unwrap();
        assert_eq!(
            upgrade.files.iter().filter(|file| file.changed()).count(),
            2
        );
        // Planning is read-only, including the manifest.
        assert_eq!(fs::read(&skill).unwrap(), old);
        upgrade.apply().unwrap();
        assert_eq!(
            fs::read(&skill).unwrap(),
            crate::relay::CONTROL_SKILL.as_bytes()
        );
        assert!(
            plan(Target::Omp, &paths, program)
                .unwrap()
                .files
                .iter()
                .all(|file| !file.changed())
        );
        fs::write(&skill, b"user edits\n").unwrap();
        assert!(plan(Target::Omp, &paths, program).is_err());
        assert!(installed_launch_at(Target::Omp, &paths, &[], None).is_err());
        assert_eq!(fs::read(&skill).unwrap(), b"user edits\n");
    }
}
