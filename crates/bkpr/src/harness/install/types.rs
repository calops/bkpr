//! Native configuration and immutable artifact descriptions for explicit installation.
use anyhow::{Context, Result, ensure};
use bkpr_protocol::HarnessKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

/// Invocation-local native configuration roots; never contains a persisted launch grant.
pub(super) struct Paths {
    /// Current account's home directory.
    pub home: PathBuf,
    /// Bkpr-owned integration data root.
    pub data: PathBuf,
    /// Absolute Bkpr executable referenced by native configuration.
    pub program: PathBuf,
    /// Invocation directory for native relative overrides.
    pub cwd: PathBuf,
    /// Frozen invocation environment, allowing isolated tests without global mutation.
    pub environment: BTreeMap<OsString, OsString>,
}
impl Paths {
    /// Resolve a native directory override, retaining invocation-relative semantics.
    pub fn env_path(&self, name: &str, fallback: PathBuf) -> Result<PathBuf> {
        let value = self
            .environment
            .get(OsStr::new(name))
            .map(PathBuf::from)
            .unwrap_or(fallback);
        ensure!(!value.as_os_str().is_empty(), "{name} must not be empty");
        Ok(if value.is_absolute() {
            value
        } else {
            self.cwd.join(value)
        })
    }

    /// Address one native profile independently of an MCP client's filtered environment.
    pub fn manifest(&self, kind: HarnessKind, native_files: &[&Path]) -> PathBuf {
        let mut files = native_files.to_vec();
        files.sort_unstable();
        files.dedup();
        let mut hash = Sha256::new();
        for file in files {
            let name = file.as_os_str().as_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
        }
        self.data
            .join(kind.as_str())
            .join("profiles")
            .join(format!("{:x}", hash.finalize()))
            .join("manifest.json")
    }
}

/// A credential-free file within an immutable integration bundle.
pub(super) struct Asset {
    /// Relative path with no traversal components.
    pub path: PathBuf,
    /// Native integration code or metadata.
    pub bytes: Cow<'static, [u8]>,
    /// Whether the file is a native executable hook.
    pub executable: bool,
}
impl Asset {
    /// Reference compile-time integration bytes without copying them.
    pub fn borrowed(path: &str, bytes: &'static [u8]) -> Self {
        Self {
            path: path.into(),
            bytes: Cow::Borrowed(bytes),
            executable: false,
        }
    }
    /// Store generated, credential-free native integration text.
    pub fn text(path: &str, text: String) -> Self {
        Self {
            path: path.into(),
            bytes: Cow::Owned(text.into_bytes()),
            executable: false,
        }
    }
}

/// Content-addressed installation assets, published before native references change.
pub(super) struct Bundle {
    /// Absolute immutable version directory.
    pub root: PathBuf,
    /// Files within this version.
    pub files: Vec<Asset>,
}
impl Bundle {
    /// Name an immutable bundle from its executable reference and complete file contents.
    pub fn new(paths: &Paths, kind: HarnessKind, mut files: Vec<Asset>) -> Result<Self> {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut hash = Sha256::new();
        for value in [
            kind.as_str().as_bytes(),
            paths.program.as_os_str().as_bytes(),
            env!("CARGO_PKG_VERSION").as_bytes(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        let mut previous = None;
        for asset in &files {
            ensure!(
                !asset.path.as_os_str().is_empty()
                    && asset
                        .path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "invalid integration asset path"
            );
            ensure!(
                previous != Some(&asset.path),
                "duplicate integration asset path"
            );
            previous = Some(&asset.path);
            let name = asset.path.as_os_str().as_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update([u8::from(asset.executable)]);
            hash.update((asset.bytes.len() as u64).to_le_bytes());
            hash.update(&asset.bytes);
        }
        let root = paths
            .data
            .join(kind.as_str())
            .join("versions")
            .join(format!("{:x}", hash.finalize()));
        Ok(Self { root, files })
    }
}

/// Native configuration syntax, edited without replacing unrelated formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Format {
    /// JSON or JSONC using the maintained concrete syntax tree.
    Json,
    /// TOML using the maintained formatting-preserving editor.
    Toml,
}

/// One owned native value or array member, independent of neighboring user settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Entry {
    /// Stable identity across program and bundle updates.
    pub id: String,
    /// Absolute native configuration path.
    pub file: PathBuf,
    /// Native syntax.
    pub format: Format,
    /// Object or table keys to the owned slot.
    pub keys: Vec<String>,
    /// Credential-free native configuration value.
    pub value: Value,
    /// Whether the slot identifies an array member rather than an entire value.
    pub array: bool,
    /// Conservative marker for detecting edited or unknown owned-looking array members.
    pub marker: Option<String>,
}
impl Entry {
    /// Describe a single owned object or table slot.
    pub fn value(id: &str, file: PathBuf, format: Format, keys: &[&str], value: Value) -> Self {
        Self {
            id: id.into(),
            file,
            format,
            keys: keys.iter().map(|key| (*key).into()).collect(),
            value,
            array: false,
            marker: None,
        }
    }
    /// Describe one owned member without replacing the surrounding native array.
    pub fn array(
        id: &str,
        file: PathBuf,
        format: Format,
        keys: &[&str],
        value: Value,
        marker: String,
    ) -> Self {
        Self {
            id: id.into(),
            file,
            format,
            keys: keys.iter().map(|key| (*key).into()).collect(),
            value,
            array: true,
            marker: Some(marker),
        }
    }
}

/// Native integration subtree removed only by explicit uninstall after ownership checks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Removal {
    /// Absolute native configuration path.
    pub file: PathBuf,
    /// Native syntax.
    pub format: Format,
    /// Logical server or hook subtree.
    pub keys: Vec<String>,
    /// Related native metadata permitted alongside the exact owned leaves.
    pub allowed: Vec<String>,
}

/// A whole installer-owned native discovery file, never a user's configuration document.
pub(super) struct OwnedFile {
    /// Stable identity used by runtime launchers to resolve the canonical discovery path.
    pub id: String,
    /// Absolute discovery path.
    pub path: PathBuf,
    /// Credential-free loader contents.
    pub bytes: Vec<u8>,
    /// Whether the loader requires executable permissions.
    pub executable: bool,
}
impl OwnedFile {
    /// Define a native text discovery file.
    pub fn text(id: &str, path: PathBuf, text: String) -> Self {
        Self {
            id: id.into(),
            path,
            bytes: text.into_bytes(),
            executable: false,
        }
    }
}

/// Complete native definition validated before any installation changes.
pub(super) struct Definition {
    /// Supported native harness.
    pub kind: HarnessKind,
    /// Immutable, credential-free native implementation.
    pub bundle: Bundle,
    /// Owned native settings.
    pub entries: Vec<Entry>,
    /// Whole owned native discovery files.
    pub files: Vec<OwnedFile>,
    /// Logical native subtrees removed on explicit uninstall.
    pub removals: Vec<Removal>,
    /// Known native disabled or managed-policy restrictions.
    pub blockers: Vec<String>,
    /// Native restart, trust and approval requirements.
    pub notes: Vec<String>,
}

/// Quote a native POSIX command hook without shell expansion of paths or arguments.
pub(super) fn shell_command(program: &Path, args: &[&str]) -> Result<String> {
    let program = program
        .to_str()
        .context("native integration executable path must be UTF-8")?;
    let mut result = String::new();
    for value in std::iter::once(program).chain(args.iter().copied()) {
        ensure!(
            !value
                .chars()
                .any(|ch| ch == '\0' || ch == '\n' || ch == '\r'),
            "native hook argument contains a control delimiter"
        );
        if !result.is_empty() {
            result.push(' ');
        }
        result.push('\'');
        result.push_str(&value.replace('\'', "'\\''"));
        result.push('\'');
    }
    Ok(result)
}

/// Read bounded native settings without invoking the harness or exposing parser contents.
pub(super) fn read_native(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    const MAX: u64 = 16 * 1024 * 1024;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read native configuration {}", path.display()));
        }
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX,
        "native configuration is not a regular file within 16 MiB: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(MAX + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX,
        "native configuration exceeds 16 MiB: {}",
        path.display()
    );
    Ok(Some(bytes))
}
