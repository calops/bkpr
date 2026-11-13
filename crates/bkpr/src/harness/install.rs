//! Explicit, profile-scoped native installation; launch authority is never persisted.
mod agy;
mod claude;
mod codex;
mod config;
mod gc;
mod omp;
mod opencode;
mod pi;
mod types;

use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::HarnessKind;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
use types::{Definition, Entry, Format, Paths, Removal};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const SCHEMA: u32 = 1;

/// Native command-line overrides applied before profile and policy discovery.
#[derive(Default)]
pub(super) struct Invocation {
    /// Omp profile selected explicitly on its native command line.
    pub profile: Option<OsString>,
    /// Native working-directory override.
    pub cwd: Option<PathBuf>,
    /// Ordered native Omp configuration overlays after environment overlays.
    pub config_files: Vec<PathBuf>,
}

/// Native loader paths retained for the lifetime of a managed launch.
pub(super) struct RuntimeInstall {
    /// Absolute ownership record for native clients with filtered environments.
    pub manifest: PathBuf,
    /// Immutable implementation root.
    pub root: PathBuf,
    _lease: Lease,
}

/// Shared public CLI definition for explicit native integration maintenance.
pub(crate) fn command() -> clap::Command {
    use clap::{Arg, ArgAction, ArgGroup};
    clap::Command::new("install")
        .about("Install a supported harness's native Bkpr integration")
        .long_about("Manage the current account/profile's native integration. Credentials remain launch-local; this command does not grant native approvals or restart harnesses. Inspect and dry-run never write files.")
        .arg(Arg::new("harness").required(true).value_parser(["claude", "codex", "omp", "pi", "opencode", "opencode2", "agy", "antigravity"]))
        .arg(Arg::new("inspect").long("inspect").action(ArgAction::SetTrue).help("Inspect configuration, ownership and known native policy restrictions"))
        .arg(Arg::new("dry-run").long("dry-run").action(ArgAction::SetTrue).help("Validate and show installation changes without writing"))
        .arg(Arg::new("update").long("update").action(ArgAction::SetTrue).help("Update a previously owned integration without replacing foreign settings"))
        .arg(Arg::new("uninstall").long("uninstall").action(ArgAction::SetTrue).help("Remove this profile's owned integration, preserving unrelated settings"))
        .group(ArgGroup::new("operation").args(["inspect", "dry-run", "update", "uninstall"]).multiple(false))
        .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print a structured installation report"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Inspect,
    DryRun,
    Install,
    Update,
    Uninstall,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredFile {
    id: String,
    path: PathBuf,
    digest: String,
    executable: bool,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    kind: HarnessKind,
    program: PathBuf,
    version: String,
    root: PathBuf,
    assets: Vec<StoredFile>,
    entries: Vec<Entry>,
    files: Vec<StoredFile>,
    removals: Vec<Removal>,
}
#[derive(Serialize)]
struct Report {
    harness: HarnessKind,
    status: String,
    installation: PathBuf,
    loaded: &'static str,
    changes: Vec<PathBuf>,
    blockers: Vec<String>,
    notes: Vec<String>,
}
struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    mode: u32,
}
struct Plan {
    changes: Vec<Change>,
    target: Manifest,
    journal: Journal,
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn paths() -> Result<Paths> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("native integration requires HOME")?;
    ensure!(
        home.is_absolute(),
        "native integration requires an absolute HOME"
    );
    let base = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    ensure!(base.is_absolute(), "XDG_DATA_HOME must be absolute");
    Ok(Paths {
        home,
        data: base.join("bkpr/integrations"),
        program: env::current_exe()?,
        cwd: env::current_dir()?,
        environment: env::vars_os().collect(),
    })
}
fn definition(paths: &Paths, kind: HarnessKind, overlays: &[PathBuf]) -> Result<Definition> {
    match kind {
        HarnessKind::Claude => claude::definition(paths),
        HarnessKind::Codex => codex::definition(paths),
        HarnessKind::Omp => omp::definition(paths, overlays),
        HarnessKind::Pi => pi::definition(paths),
        HarnessKind::Opencode => opencode::definition(paths),
        HarnessKind::Agy => agy::definition(paths),
        _ => {
            bail!("durable installation is supported for claude, codex, omp, pi, opencode and agy")
        }
    }
}
fn location(paths: &Paths, definition: &Definition) -> PathBuf {
    let files: Vec<_> = definition
        .entries
        .iter()
        .map(|entry| entry.file.as_path())
        .chain(definition.files.iter().map(|file| file.path.as_path()))
        .collect();
    paths.manifest(definition.kind, &files)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn stored(definition: &Definition, program: &Path) -> Manifest {
    Manifest {
        schema: SCHEMA,
        kind: definition.kind,
        program: program.to_owned(),
        version: env!("CARGO_PKG_VERSION").into(),
        root: definition.bundle.root.clone(),
        assets: definition
            .bundle
            .files
            .iter()
            .map(|file| StoredFile {
                id: file.path.to_string_lossy().into_owned(),
                path: definition.bundle.root.join(&file.path),
                digest: digest(&file.bytes),
                executable: file.executable,
            })
            .collect(),
        entries: definition.entries.clone(),
        files: definition
            .files
            .iter()
            .map(|file| StoredFile {
                id: file.id.clone(),
                path: file.path.clone(),
                digest: digest(&file.bytes),
                executable: file.executable,
            })
            .collect(),
        removals: definition.removals.clone(),
    }
}

fn safe_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir)),
        "integration path must be absolute without parent traversal"
    );
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "refusing symlink integration path {}",
                    ancestor.display()
                );
                if ancestor != path {
                    ensure!(
                        metadata.is_dir(),
                        "integration ancestor is not a directory: {}",
                        ancestor.display()
                    );
                }
                if metadata.is_dir() {
                    let uid = metadata.uid();
                    let mode = metadata.permissions().mode();
                    ensure!(
                        (uid == 0 || uid == unsafe { libc::geteuid() })
                            && (mode & 0o022 == 0 || uid == 0 && mode & 0o1000 != 0),
                        "unsafe integration directory {}",
                        ancestor.display()
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect integration path {}", ancestor.display()));
            }
        }
    }
    Ok(())
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    safe_path(path)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read integration file {}", path.display()));
        }
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.permissions().mode() & 0o022 == 0
            && metadata.len() <= MAX_FILE,
        "unsafe or oversized integration file {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE,
        "integration file exceeds 16 MiB: {}",
        path.display()
    );
    Ok(Some(bytes))
}
fn directory(path: &Path, private: bool) -> Result<()> {
    safe_path(path)?;
    let missing: Vec<_> = path.ancestors().take_while(|path| !path.exists()).collect();
    for directory in missing.into_iter().rev() {
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                safe_path(directory)?
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create integration directory {}", directory.display())
                });
            }
        }
        sync_directory(
            directory
                .parent()
                .context("integration directory has no parent")?,
        )?;
    }
    let metadata = fs::metadata(path)?;
    ensure!(
        metadata.is_dir(),
        "integration path is not a directory: {}",
        path.display()
    );
    if private {
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            "integration directory must be private and owned: {}",
            path.display()
        );
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("sync integration directory {}", path.display()))
}
fn write(path: &Path, bytes: Option<&[u8]>, mode: u32) -> Result<()> {
    safe_path(path)?;
    let parent = path.parent().context("integration file has no parent")?;
    directory(parent, false)?;
    if let Some(bytes) = bytes {
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("publish integration file {}", path.display()))?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    sync_directory(parent)
}
fn private_file(path: &Path, create: bool) -> Result<File> {
    safe_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.permissions().mode() & 0o077 == 0,
        "unsafe integration lease {}",
        path.display()
    );
    Ok(file)
}
fn ownership<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if let Err(error) = fs::symlink_metadata(path) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    let Some(bytes) = read(path)? else {
        return Ok(None);
    };
    let parent = path.parent().context("invalid installation record path")?;
    let directory = fs::metadata(parent)?;
    ensure!(
        directory.uid() == unsafe { libc::geteuid() }
            && directory.permissions().mode() & 0o077 == 0
            && fs::metadata(path)?.permissions().mode() & 0o077 == 0,
        "installation ownership must be private and owned"
    );
    Ok(Some(serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("invalid installation ownership record {}", path.display())
    })?))
}
fn validate_scope(path: &Path, kind: HarnessKind, manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema == SCHEMA && manifest.kind == kind,
        "unsupported or mismatched installation ownership record"
    );
    let data = path
        .ancestors()
        .nth(4)
        .context("invalid installation ownership path")?;
    let reference = Paths {
        home: PathBuf::from("/"),
        data: data.to_owned(),
        program: manifest.program.clone(),
        cwd: PathBuf::from("/"),
        environment: BTreeMap::new(),
    };
    let files: Vec<_> = manifest
        .entries
        .iter()
        .map(|entry| entry.file.as_path())
        .chain(manifest.files.iter().map(|file| file.path.as_path()))
        .collect();
    ensure!(
        reference.manifest(kind, &files) == path.with_file_name("manifest.json")
            && manifest.root.parent() == Some(data.join(kind.as_str()).join("versions").as_path()),
        "installation ownership record has an invalid scope"
    );
    for asset in &manifest.assets {
        ensure!(
            asset.path.starts_with(&manifest.root)
                && !asset
                    .path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)),
            "installation asset escaped its immutable root"
        );
    }
    Ok(())
}
fn record(path: &Path, kind: HarnessKind) -> Result<Option<Manifest>> {
    let manifest = ownership(path)?;
    if let Some(manifest) = &manifest {
        validate_scope(path, kind, manifest)?;
    }
    Ok(manifest)
}

#[derive(Serialize, Deserialize)]
struct Journal {
    owners: Vec<Manifest>,
}

fn journal(path: &Path, kind: HarnessKind) -> Result<Option<Journal>> {
    let result: Option<Journal> = ownership(path)?;
    if let Some(journal) = &result {
        ensure!(
            !journal.owners.is_empty(),
            "empty installation ownership journal"
        );
        for owner in &journal.owners {
            validate_scope(path, kind, owner)?;
        }
    }
    Ok(result)
}
fn matches_file(file: &StoredFile) -> Result<bool> {
    let Some(bytes) = read(&file.path)? else {
        return Ok(false);
    };
    let metadata = fs::metadata(&file.path)?;
    Ok(digest(&bytes) == file.digest
        && (metadata.permissions().mode() & 0o100 != 0) == file.executable)
}
fn contains_marker(value: &Value, marker: &str) -> bool {
    match value {
        Value::String(text) => text.contains(marker),
        Value::Array(values) => values.iter().any(|value| contains_marker(value, marker)),
        Value::Object(values) => values.values().any(|value| contains_marker(value, marker)),
        _ => false,
    }
}
fn member(
    document: &config::Document,
    entries: &[&Entry],
    known: &[&Entry],
) -> Result<Option<usize>> {
    let entry = entries
        .first()
        .context("missing native member definition")?;
    let Some(value) = document.get(&entry.keys)? else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .context("native integration list is not an array")?;
    let mut found = None;
    for (index, value) in values.iter().enumerate() {
        if entries.iter().any(|entry| value == &entry.value) {
            ensure!(
                found.is_none(),
                "duplicate owned integration member: {}",
                entry.id
            );
            found = Some(index);
        } else if known.iter().any(|sibling| {
            sibling.file == entry.file
                && sibling.keys == entry.keys
                && sibling.array
                && value == &sibling.value
        }) {
            continue;
        } else if entries.iter().any(|entry| {
            entry
                .marker
                .as_deref()
                .is_some_and(|marker| contains_marker(value, marker))
        }) {
            bail!("modified or foreign integration member: {}", entry.id);
        }
    }
    Ok(found)
}
fn check_entry(document: &config::Document, entry: &Entry, known: &[&Entry]) -> Result<bool> {
    if entry.array {
        Ok(member(document, &[entry], known)?.is_some())
    } else {
        Ok(document.get(&entry.keys)?.as_ref() == Some(&entry.value))
    }
}
fn verify_record(manifest: &Manifest) -> Result<()> {
    for file in manifest.assets.iter().chain(&manifest.files) {
        ensure!(
            matches_file(file)?,
            "installed file changed or disappeared: {}",
            file.path.display()
        );
    }
    let mut documents = BTreeMap::new();
    let known: Vec<_> = manifest.entries.iter().collect();
    for entry in &manifest.entries {
        if !documents.contains_key(&entry.file) {
            let before = read(&entry.file)?;
            documents.insert(
                entry.file.clone(),
                config::Document::parse(entry.format, before.as_deref(), &entry.file)?,
            );
        }
        ensure!(
            check_entry(&documents[&entry.file], entry, &known)?,
            "installed native setting changed or disappeared: {} ({})",
            entry.file.display(),
            entry.id
        );
    }
    Ok(())
}

/// Resolve the owned current-profile installation, rejecting stale code, edits and native opt-outs.
pub(super) fn runtime(kind: HarnessKind, invocation: Invocation) -> Result<Option<RuntimeInstall>> {
    let mut paths = paths()?;
    if let Some(profile) = invocation.profile {
        ensure!(
            kind == HarnessKind::Omp,
            "native profile override is Omp-only"
        );
        paths.environment.insert("OMP_PROFILE".into(), profile);
    }
    if let Some(cwd) = invocation.cwd {
        let cwd = if cwd == Path::new("~") {
            paths.home.clone()
        } else if let Ok(relative) = cwd.strip_prefix("~/") {
            paths.home.join(relative)
        } else {
            cwd
        };
        paths.cwd = fs::canonicalize(if cwd.is_absolute() {
            cwd
        } else {
            paths.cwd.join(cwd)
        })
        .context("resolve native working directory")?;
        ensure!(
            paths.cwd.is_dir(),
            "native working directory is not a directory"
        );
    }
    let definition = definition(&paths, kind, &invocation.config_files)?;
    let path = location(&paths, &definition);
    ensure!(
        !path.with_file_name("pending.json").exists(),
        "integration update is incomplete; run bkpr install {} --update",
        kind.as_str()
    );
    let Some(manifest) = record(&path, kind)? else {
        if legacy_agy(&definition, &paths)?.is_none() {
            check_unowned(&definition)?;
        }
        return Ok(None);
    };
    ensure!(
        definition.blockers.is_empty(),
        "native integration is disabled or restricted: {}",
        definition.blockers.join("; ")
    );
    let expected = stored(&definition, &paths.program);
    ensure!(
        manifest == expected,
        "installed integration is stale; run bkpr install {} --update",
        kind.as_str()
    );
    verify_record(&manifest)?;
    let file = private_file(&manifest.root.join(".lease"), false)?;
    FileExt::lock_shared(&file)?;
    Ok(Some(RuntimeInstall {
        manifest: path,
        root: manifest.root,
        _lease: Lease(file),
    }))
}

/// Validate a persistent native command's explicit account/profile ownership reference.
pub(super) fn validate_command(path: &Path, kind: HarnessKind) -> Result<()> {
    let manifest =
        record(path, kind)?.context("integration ownership record is missing; install it again")?;
    ensure!(
        manifest.program == env::current_exe()? && manifest.version == env!("CARGO_PKG_VERSION"),
        "installed command is stale; update the integration"
    );
    ensure!(
        !path.with_file_name("pending.json").exists(),
        "integration update is incomplete; run bkpr install {} --update",
        kind.as_str()
    );
    for file in &manifest.assets {
        ensure!(
            matches_file(file)?,
            "installed implementation changed or disappeared: {}",
            file.path.display()
        );
    }
    Ok(())
}

fn plan(definition: &Definition, paths: &Paths, owners: &[Manifest], mode: Mode) -> Result<Plan> {
    let target = stored(definition, &paths.program);
    let removing = mode == Mode::Uninstall;
    let mut changes = Vec::new();
    let mut used_owners = BTreeSet::new();
    let mut formats = BTreeMap::new();
    for entry in target
        .entries
        .iter()
        .chain(owners.iter().flat_map(|old| &old.entries))
    {
        if let Some(format) = formats.insert(entry.file.clone(), entry.format) {
            ensure!(
                format == entry.format,
                "conflicting native document formats"
            );
        }
    }
    for removal in &definition.removals {
        formats
            .entry(removal.file.clone())
            .or_insert(removal.format);
    }
    for (file, format) in formats {
        let before = read(&file)?;
        let mut document = config::Document::parse(format, before.as_deref(), &file)?;
        if owners.is_empty() {
            for removal in definition
                .removals
                .iter()
                .filter(|removal| removal.file == file)
            {
                ensure!(
                    document.get(&removal.keys)?.is_none(),
                    "foreign native integration subtree: {}",
                    file.display()
                );
            }
        }
        let mut old_entries: BTreeMap<&str, Vec<(&Entry, usize)>> = BTreeMap::new();
        for (index, owner) in owners.iter().enumerate() {
            for entry in owner.entries.iter().filter(|entry| entry.file == file) {
                old_entries
                    .entry(&entry.id)
                    .or_default()
                    .push((entry, index));
            }
        }
        let new_entries: BTreeMap<_, _> = target
            .entries
            .iter()
            .filter(|entry| entry.file == file)
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let known: Vec<_> = target
            .entries
            .iter()
            .chain(owners.iter().flat_map(|owner| &owner.entries))
            .filter(|entry| entry.file == file)
            .collect();
        let ids: BTreeSet<_> = old_entries
            .keys()
            .chain(new_entries.keys())
            .copied()
            .collect();
        for id in ids {
            let old = old_entries.get(id).map(Vec::as_slice).unwrap_or_default();
            let new = new_entries.get(id).copied();
            let entry = new
                .or_else(|| old.first().map(|(entry, _)| *entry))
                .context("missing installation entry")?;
            let mut candidates = vec![entry];
            for (old, _) in old {
                ensure!(
                    old.file == entry.file && old.keys == entry.keys && old.array == entry.array,
                    "unsupported native setting ownership migration"
                );
                candidates.push(old);
            }
            let desired = if removing {
                None
            } else {
                new.map(|entry| &entry.value)
            };
            let index = if entry.array {
                member(&document, &candidates, &known)?
            } else {
                None
            };
            let current = if entry.array {
                let value = document.get(&entry.keys)?;
                index.and_then(|index| {
                    value.and_then(|value| {
                        value
                            .as_array()
                            .and_then(|values| values.get(index).cloned())
                    })
                })
            } else {
                document.get(&entry.keys)?
            };
            ensure!(
                current.is_none()
                    || !old.is_empty()
                        && candidates
                            .iter()
                            .any(|entry| current.as_ref() == Some(&entry.value)),
                "foreign or modified native setting: {} ({})",
                file.display(),
                entry.id
            );
            if let Some((_, index)) = old
                .iter()
                .rev()
                .find(|(entry, _)| current.as_ref() == Some(&entry.value))
            {
                used_owners.insert(*index);
            }
            if current.as_ref() != desired {
                if entry.array {
                    document.array_change(&entry.keys, index, desired)?;
                } else {
                    document.set(&entry.keys, desired)?;
                }
            }
        }
        if removing && !owners.is_empty() {
            for removal in definition
                .removals
                .iter()
                .filter(|removal| removal.file == file)
            {
                if let Some(value) = document.get(&removal.keys)? {
                    let table = value
                        .as_object()
                        .context("native integration subtree is not an object")?;
                    ensure!(
                        table.keys().all(|key| removal.allowed.contains(key)),
                        "foreign settings in native integration subtree: {}",
                        file.display()
                    );
                    document.set(&removal.keys, None)?;
                }
            }
        }
        let after = document.bytes();
        if before.as_deref() != Some(&after) && !(before.is_none() && removing) {
            let mode = fs::metadata(&file)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0o600);
            changes.push(Change {
                path: file,
                before,
                after: Some(after),
                mode,
            });
        }
    }
    let mut old_files: BTreeMap<&str, Vec<(&StoredFile, usize)>> = BTreeMap::new();
    for (index, owner) in owners.iter().enumerate() {
        for file in &owner.files {
            old_files.entry(&file.id).or_default().push((file, index));
        }
    }
    let new_files: BTreeMap<_, _> = definition
        .files
        .iter()
        .map(|file| (file.id.as_str(), file))
        .collect();
    let ids: BTreeSet<_> = old_files.keys().chain(new_files.keys()).copied().collect();
    for id in ids {
        let old = old_files.get(id).map(Vec::as_slice).unwrap_or_default();
        let new = new_files.get(id).copied();
        let path = new
            .map(|file| &file.path)
            .or_else(|| old.first().map(|(file, _)| &file.path))
            .context("missing owned native file")?;
        ensure!(
            old.iter().all(|(file, _)| &file.path == path),
            "unsupported native file ownership migration"
        );
        let before = read(path)?;
        let after = if removing {
            None
        } else {
            new.map(|file| file.bytes.clone())
        };
        if let Some(bytes) = &before {
            let hash = digest(bytes);
            let matched = old.iter().rev().find(|(file, _)| file.digest == hash);
            ensure!(
                matched.is_some() || !old.is_empty() && before == after,
                "foreign or modified native discovery file {}",
                path.display()
            );
            if let Some((_, index)) = matched {
                used_owners.insert(*index);
            }
        }
        if before != after {
            changes.push(Change {
                path: path.clone(),
                before,
                after,
                mode: if new.is_some_and(|file| file.executable) {
                    0o700
                } else {
                    0o600
                },
            });
        }
    }
    let mut journal_owners: Vec<_> = used_owners
        .into_iter()
        .map(|index| owners[index].clone())
        .collect();
    if !removing {
        journal_owners.push(target.clone());
    } else if journal_owners.is_empty()
        && let Some(owner) = owners.last()
    {
        journal_owners.push(owner.clone());
    }
    Ok(Plan {
        changes,
        target,
        journal: Journal {
            owners: journal_owners,
        },
    })
}

fn publish_bundle(definition: &Definition) -> Result<()> {
    directory(&definition.bundle.root, true)?;
    let assets: Vec<_> = definition
        .bundle
        .files
        .iter()
        .map(|asset| StoredFile {
            id: asset.path.to_string_lossy().into_owned(),
            path: definition.bundle.root.join(&asset.path),
            digest: digest(&asset.bytes),
            executable: asset.executable,
        })
        .collect();
    let bytes = serde_json::to_vec(&assets)?;
    let record = definition.bundle.root.join(".bundle.json");
    if let Some(before) = read(&record)? {
        ensure!(
            before == bytes,
            "immutable integration bundle record was modified"
        );
    } else {
        ensure!(
            fs::read_dir(&definition.bundle.root)?.next().is_none(),
            "unowned contents in integration bundle"
        );
        write(&record, Some(&bytes), 0o600)?;
    }
    for asset in &definition.bundle.files {
        let path = definition.bundle.root.join(&asset.path);
        match read(&path)? {
            Some(bytes) => ensure!(
                bytes == asset.bytes.as_ref(),
                "immutable integration asset was modified: {}",
                path.display()
            ),
            None => write(
                &path,
                Some(&asset.bytes),
                if asset.executable { 0o700 } else { 0o600 },
            )?,
        }
    }
    let _ = private_file(&definition.bundle.root.join(".lease"), true)?;
    sync_directory(&definition.bundle.root)
}
fn commit(plan: &Plan, manifest_path: &Path, removing: bool) -> Result<()> {
    let pending = manifest_path.with_file_name("pending.json");
    let previous_journal = read(&pending)?;
    let journal = serde_json::to_vec(&plan.journal)?;
    ensure!(
        journal.len() as u64 <= MAX_FILE,
        "integration ownership journal exceeds 16 MiB"
    );
    let metadata = Change {
        path: manifest_path.to_owned(),
        before: read(manifest_path)?,
        after: if removing {
            None
        } else {
            Some(serde_json::to_vec(&plan.target)?)
        },
        mode: 0o600,
    };
    write(&pending, Some(&journal), 0o600)?;
    let mut applied: Vec<&Change> = Vec::new();
    let result: Result<()> = (|| {
        for change in plan.changes.iter().chain(std::iter::once(&metadata)) {
            ensure!(
                read(&change.path)? == change.before,
                "native configuration changed concurrently: {}",
                change.path.display()
            );
            // A rename followed by failed directory fsync still needs conditional rollback.
            applied.push(change);
            write(&change.path, change.after.as_deref(), change.mode)?;
        }
        write(&pending, None, 0o600)?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut incomplete = Vec::new();
        for change in applied.into_iter().rev() {
            match read(&change.path) {
                Ok(current) if current == change.after => {
                    if write(&change.path, change.before.as_deref(), change.mode).is_err() {
                        incomplete.push(change.path.clone());
                    }
                }
                Ok(current) if current == change.before => {}
                _ => incomplete.push(change.path.clone()),
            }
        }
        if incomplete.is_empty() {
            let cleanup = write(&pending, previous_journal.as_deref(), 0o600);
            return Err(error).context(if cleanup.is_ok() {"installation did not commit; native changes and ownership metadata rolled back to their previous state"} else {"installation did not commit; native changes rolled back, but ownership journal restoration was not confirmed durable"});
        }
        let retained = write(&pending, Some(&journal), 0o600).is_ok();
        bail!(
            "installation did not commit: {error:#}; conditional rollback could not restore {} file(s). {}",
            incomplete.len(),
            if retained {
                "Ownership journal retained; use --update or --uninstall to repair exact owned values"
            } else {
                "Ownership journal could not be confirmed durable; manually inspect native configuration before retrying"
            }
        );
    }
    Ok(())
}

/// Run explicit native installation maintenance without starting any harness.
pub(super) fn run(arguments: Vec<OsString>) -> Result<i32> {
    let matches = match command()
        .try_get_matches_from(std::iter::once(OsString::from("bkpr install")).chain(arguments))
    {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(code);
        }
    };
    let name = matches
        .get_one::<String>("harness")
        .context("missing native harness")?;
    let kind = match name.as_str() {
        "claude" => HarnessKind::Claude,
        "codex" => HarnessKind::Codex,
        "omp" => HarnessKind::Omp,
        "pi" => HarnessKind::Pi,
        "opencode" | "opencode2" => HarnessKind::Opencode,
        "agy" | "antigravity" => HarnessKind::Agy,
        _ => unreachable!(),
    };
    let mode = if matches.get_flag("inspect") {
        Mode::Inspect
    } else if matches.get_flag("dry-run") {
        Mode::DryRun
    } else if matches.get_flag("uninstall") {
        Mode::Uninstall
    } else if matches.get_flag("update") {
        Mode::Update
    } else {
        Mode::Install
    };
    let paths = paths()?;
    let definition = definition(&paths, kind, &[])?;
    let manifest_path = location(&paths, &definition);
    let mut report = Report {
        harness: kind,
        status: "not_installed".into(),
        installation: manifest_path.clone(),
        loaded: "unknown",
        changes: Vec::new(),
        blockers: definition.blockers.clone(),
        notes: definition.notes.clone(),
    };
    let readonly = matches!(mode, Mode::Inspect | Mode::DryRun);
    if mode == Mode::Uninstall
        && !paths.data.join(kind.as_str()).exists()
        && legacy_agy(&definition, &paths)?.is_none()
    {
        print_report(&report, matches.get_flag("json"))?;
        return Ok(0);
    }
    let _lock = if readonly {
        None
    } else {
        let root = paths.data.join(kind.as_str());
        directory(&root, true)?;
        let file = private_file(&root.join("install.lock"), true)?;
        FileExt::lock_exclusive(&file)?;
        Some(Lease(file))
    };
    let previous = record(&manifest_path, kind)?;
    let pending_path = manifest_path.with_file_name("pending.json");
    let pending = journal(&pending_path, kind)?;
    let interrupted = pending.is_some();
    let legacy = if previous.is_none() && pending.is_none() {
        legacy_agy(&definition, &paths)?
    } else {
        None
    };
    let owners = pending
        .as_ref()
        .map(|pending| pending.owners.clone())
        .unwrap_or_else(|| previous.iter().chain(legacy.iter()).cloned().collect());
    if interrupted {
        report.notes.push("A previous operation did not confirm its commit; use --update or --uninstall to repair exact owned values.".into());
    }
    if legacy.is_some() {
        report.notes.push(
            "An exact legacy Bkpr AGY registration can be migrated to owned durable installation."
                .into(),
        );
    }
    if mode != Mode::Uninstall && !definition.blockers.is_empty() {
        report.status = "blocked".into();
        print_report(&report, matches.get_flag("json"))?;
        return Ok(1);
    }
    if mode == Mode::Uninstall && owners.is_empty() {
        match gc::collect(&paths, kind, true) {
            Ok(notes) => report.notes.extend(notes),
            Err(_) => report.notes.push("Obsolete-bundle cleanup could not be completed safely; existing files were retained.".into()),
        }
        print_report(&report, matches.get_flag("json"))?;
        return Ok(0);
    }
    if interrupted && !matches!(mode, Mode::Update | Mode::Uninstall | Mode::Inspect) {
        bail!("integration operation is incomplete; use --update or --uninstall");
    }
    let plan = match plan(&definition, &paths, &owners, mode) {
        Ok(plan) => plan,
        Err(error) if mode == Mode::Inspect => {
            report.status = "conflict".into();
            report.notes.push(format!("{error:#}"));
            print_report(&report, matches.get_flag("json"))?;
            return Ok(1);
        }
        Err(error) => return Err(error),
    };
    report.changes = plan
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect();
    let mut current = previous.as_ref() == Some(&plan.target);
    if mode != Mode::Uninstall
        && let Some(previous) = &previous
    {
        for file in &previous.assets {
            current &= matches_file(file)?;
        }
    }
    report.status = if interrupted {
        "incomplete"
    } else if owners.is_empty() {
        "not_installed"
    } else if current && plan.changes.is_empty() {
        "configured"
    } else {
        "update_required"
    }
    .into();
    if readonly {
        print_report(&report, matches.get_flag("json"))?;
        return Ok(i32::from(interrupted));
    }
    if mode == Mode::Install
        && !owners.is_empty()
        && legacy.is_none()
        && report.status != "configured"
    {
        bail!("integration has changed; inspect it and use --update");
    }
    if mode == Mode::Install && report.status == "configured" {
        print_report(&report, matches.get_flag("json"))?;
        return Ok(0);
    }
    directory(
        manifest_path
            .parent()
            .context("installation record has no directory")?,
        true,
    )?;
    if mode != Mode::Uninstall {
        publish_bundle(&definition)?;
    }
    commit(&plan, &manifest_path, mode == Mode::Uninstall)?;
    match gc::collect(&paths, kind, mode == Mode::Uninstall) {
        Ok(notes) => report.notes.extend(notes),
        Err(_) => report.notes.push("Native commit succeeded; obsolete-bundle cleanup could not be completed safely. Existing files were retained.".into()),
    }
    report.status = if mode == Mode::Uninstall {
        "uninstalled"
    } else {
        "configured"
    }
    .into();
    report.notes.push("Native configuration is durable; already running harnesses were not restarted. No approval or trust decision was granted.".into());
    print_report(&report, matches.get_flag("json"))?;
    Ok(0)
}
fn print_report(report: &Report, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(report)?);
    } else {
        println!("{} integration: {}", report.harness.as_str(), report.status);
        for path in &report.changes {
            println!("  {}", path.display());
        }
        for blocker in &report.blockers {
            println!("Blocked: {blocker}");
        }
        for note in &report.notes {
            println!("{note}");
        }
        println!(
            "Loaded in a running harness: unknown (this command does not start or inspect native sessions)."
        );
    }
    Ok(())
}

/// Read native preferences with their native symlink semantics and bounded allocation.
pub(super) fn read_native_config(path: &Path) -> Result<Option<Vec<u8>>> {
    types::read_native(path)
}

/// Route durable native commands through the same authenticated callbacks and MCP.
pub(super) fn bridge(arguments: Vec<OsString>, hook: bool) -> Result<i32> {
    use clap::{Arg, Command};
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(if hook {
        "capture installed-hook"
    } else {
        "capture installed-mcp"
    })
    .arg(
        Arg::new("harness")
            .long("harness")
            .required(true)
            .value_parser(["claude", "codex", "omp", "pi", "opencode", "agy"]),
    )
    .arg(
        Arg::new("installation")
            .long("installation")
            .required(true)
            .value_parser(clap::value_parser!(PathBuf)),
    );
    if hook {
        command = command.arg(
            Arg::new("callback")
                .long("callback")
                .required(true)
                .value_parser(["claude-hook", "claude-wait"]),
        );
    }
    let matches = command
        .try_get_matches_from(std::iter::once(OsString::from("bkpr capture")).chain(arguments))?;
    let kind = HarnessKind::from_command(
        matches
            .get_one::<String>("harness")
            .context("missing installed harness")?,
    )
    .context("unsupported installed harness")?;
    ensure!(
        !hook || kind == HarnessKind::Claude,
        "installed command hooks are Claude-only"
    );
    let hint = env::var_os("BKPR_INTEGRATION_FILE");
    let relay = ["BKPR_AGENT_RELAY_SOCKET", "BKPR_AGENT_RELAY_TOKEN"]
        .iter()
        .any(|name| env::var_os(name).is_some());
    let capture_context = [
        "BKPR_CAPTURE_LAUNCH_ID",
        "BKPR_CAPTURE_TOKEN",
        "BKPR_CAPTURE_HARNESS",
        "BKPR_CAPTURE_PROGRAM",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some());
    if hook && hint.is_none() && !capture_context && !relay {
        return Ok(0);
    }
    validate_command(
        matches
            .get_one::<PathBuf>("installation")
            .context("missing installed ownership record")?,
        kind,
    )?;
    if let Some(path) = hint {
        let path = PathBuf::from(path);
        if !hook {
            return super::assets::exec_mcp(&path, kind);
        }
        let capture = super::assets::read_credentials(&path, kind)?;
        let mut command = std::process::Command::new(env::current_exe()?);
        capture.apply(&mut command);
        command.args([
            "capture",
            matches
                .get_one::<String>("callback")
                .context("missing installed hook callback")?,
        ]);
        return Err(command.exec()).context("execute authenticated native hook");
    }
    if relay {
        ensure!(
            kind == HarnessKind::Codex && !hook,
            "unexpected relay context for installed integration"
        );
        return super::agent::mcp(kind);
    }
    if capture_context {
        let capture = super::Capture::from_environment()?;
        ensure!(
            capture.grant.kind == kind,
            "installed integration does not match launch context"
        );
        if hook {
            return match matches.get_one::<String>("callback").map(String::as_str) {
                Some("claude-hook") => {
                    super::claude::hook()?;
                    Ok(0)
                }
                Some("claude-wait") => super::claude::wait(),
                _ => bail!("unsupported installed native callback"),
            };
        }
        return if kind == HarnessKind::Agy {
            super::agy::mcp()
        } else {
            super::agent::mcp(kind)
        };
    }
    super::agent::inactive_mcp(kind)
}

fn legacy_agy(definition: &Definition, paths: &Paths) -> Result<Option<Manifest>> {
    if definition.kind != HarnessKind::Agy {
        return Ok(None);
    }
    let file = &definition
        .entries
        .first()
        .context("missing Antigravity registration")?
        .file;
    let before = types::read_native(file)?;
    let document = config::Document::parse(Format::Json, before.as_deref(), file)?;
    let keys = vec!["mcpServers".into(), "bkpr".into()];
    let Some(value) = document.get(&keys)? else {
        return Ok(None);
    };
    let expected_args = serde_json::json!(["capture", "mcp", "--harness", "agy"]);
    if value.get("command") != Some(&serde_json::to_value(&paths.program)?)
        || value.get("args") != Some(&expected_args)
    {
        return Ok(None);
    }
    let table = value
        .as_object()
        .context("legacy Antigravity MCP entry is not an object")?;
    ensure!(
        table
            .keys()
            .all(|key| matches!(key.as_str(), "command" | "args" | "disabled")),
        "foreign settings in legacy Antigravity integration"
    );
    let mut legacy = stored(definition, &paths.program);
    legacy.assets.clear();
    legacy
        .entries
        .iter_mut()
        .find(|entry| entry.id == "args")
        .context("missing legacy Antigravity arguments")?
        .value = expected_args;
    Ok(Some(legacy))
}

#[cfg(test)]
mod tests;

fn check_unowned(definition: &Definition) -> Result<()> {
    let mut documents = BTreeMap::new();
    for entry in &definition.entries {
        if !documents.contains_key(&entry.file) {
            let bytes = types::read_native(&entry.file)?;
            documents.insert(
                entry.file.clone(),
                config::Document::parse(entry.format, bytes.as_deref(), &entry.file)?,
            );
        }
        let document = &documents[&entry.file];
        let value = document.get(&entry.keys)?;
        let occupied = if entry.array {
            value
                .as_ref()
                .map(|value| {
                    value
                        .as_array()
                        .context("native plugin configuration must be an array")
                })
                .transpose()?
                .is_some_and(|values| {
                    values.iter().any(|value| {
                        value == &entry.value
                            || entry
                                .marker
                                .as_deref()
                                .is_some_and(|marker| contains_marker(value, marker))
                    })
                })
        } else {
            value.is_some()
        };
        ensure!(
            !occupied,
            "unowned native Bkpr configuration: {}; inspect with bkpr install {} --inspect",
            entry.file.display(),
            definition.kind.as_str()
        );
    }
    for removal in &definition.removals {
        if let Some(document) = documents.get(&removal.file) {
            ensure!(
                document.get(&removal.keys)?.is_none(),
                "unowned native Bkpr server configuration: {}",
                removal.file.display()
            );
        }
    }
    for file in &definition.files {
        match fs::symlink_metadata(&file.path) {
            Ok(_) => bail!(
                "unowned native Bkpr discovery file: {}",
                file.path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect native discovery path"),
        }
    }
    Ok(())
}
