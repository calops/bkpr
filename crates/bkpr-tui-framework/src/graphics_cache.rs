//! Worker-only, best-effort storage for reusable chrome PNG bytes.

use rustix::fs::{self as filesystem, AtFlags, Dir, DirEntry, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const TOUCH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const LOCK_NAME: &CStr = c".lock";
const PENDING_PREFIX: &str = ".pending-";
const PENDING_SUFFIX: &str = ".tmp";
const PENDING_RANDOM_LEN: usize = 16;
const FILE_FLAGS: OFlags = OFlags::NOFOLLOW
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const READ_FLAGS: OFlags = FILE_FLAGS.union(OFlags::RDONLY);

/// A shared, bounded PNG byte cache; call its operations only on preparation workers.
///
/// Filesystem failures and contention are cache misses, never rendering failures.
/// Callers own PNG validation and the versioned visual-input digest used as a key.
pub(crate) struct DiskCache {
    root: PathBuf,
    limits: Limits,
}

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    entries: usize,
}

impl DiskCache {
    /// Selects a private per-user managed directory without performing filesystem I/O.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            limits: Limits {
                bytes: MAX_BYTES,
                entries: MAX_ENTRIES,
            },
        }
    }

    /// Reads at most `max_bytes` bytes, coalescing persisted LRU touches to once hourly.
    ///
    /// Oversized, unsafe, unavailable or concurrently busy entries are cache misses.
    pub(crate) fn get(&mut self, key: &[u8; 32], max_bytes: usize) -> Option<Vec<u8>> {
        let locked = LockedRoot::acquire(&self.root).ok()?;
        let name = key_name(key);
        let name = name.as_c_str();
        let mut file = File::from(
            filesystem::openat(&locked.directory, name, READ_FLAGS, Mode::empty()).ok()?,
        );
        let metadata = private_file(&file).ok()?;
        let mut inventory = locked.inventory().ok()?;
        locked.trim(&mut inventory, self.limits, 0, 0).ok()?;
        let entry = inventory
            .entries
            .iter()
            .find(|entry| entry.name() == name)?;
        if !same_file(&metadata, &entry.metadata) || metadata.len() != entry.metadata.len() {
            return None;
        }
        let length = usize::try_from(metadata.len()).ok()?;
        if length > max_bytes {
            return None;
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).ok()?;
        bytes.resize(length, 0);
        file.read_exact(&mut bytes).ok()?;
        if file.read(&mut [0]).ok()? != 0 {
            return None;
        }
        let now = SystemTime::now();
        if metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= TOUCH_INTERVAL)
        {
            let _ = file.set_modified(now);
        }
        Some(bytes)
    }

    /// Atomically admits bytes after reserving shared space, returning false on bypass.
    ///
    /// The total budget includes the complete pending file and any replaced entry.
    /// This does not validate PNG contents or impose a separate per-entry size limit.
    pub(crate) fn put(&mut self, key: &[u8; 32], bytes: &[u8]) -> bool {
        self.try_put(key, bytes).is_ok()
    }

    /// Removes an existing managed entry, ignoring contention and filesystem failures.
    pub(crate) fn remove(&mut self, key: &[u8; 32]) {
        let _ = self.try_remove(key);
    }

    fn try_put(&self, key: &[u8; 32], bytes: &[u8]) -> io::Result<()> {
        let length = u64::try_from(bytes.len()).map_err(|_| invalid())?;
        if length > self.limits.bytes || self.limits.entries == 0 {
            return Err(invalid());
        }
        let locked = LockedRoot::acquire(&self.root)?;
        let mut inventory = locked.inventory()?;
        // Reserve for both files during replacement, including the pending entry.
        locked.trim(&mut inventory, self.limits, length, 1)?;
        let mut pending = PendingFile::new(&locked.directory, &self.root)?;
        pending.file.as_file_mut().write_all(bytes)?;
        pending.file.as_file_mut().flush()?;
        let name = key_name(key);
        let name = name.as_c_str();
        filesystem::renameat(&locked.directory, &pending.name, &locked.directory, name)?;
        pending.published = true;
        Ok(())
    }

    fn try_remove(&self, key: &[u8; 32]) -> io::Result<()> {
        let locked = LockedRoot::acquire(&self.root)?;
        let inventory = locked.inventory()?;
        let name = key_name(key);
        let name = name.as_c_str();
        if let Some(entry) = inventory.entries.iter().find(|entry| entry.name() == name) {
            locked.remove(entry.name(), &entry.metadata)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn with_limits(root: PathBuf, bytes: u64, entries: usize) -> Self {
        Self {
            root,
            limits: Limits { bytes, entries },
        }
    }
}

struct LockedRoot {
    directory: File,
    lock: File,
}

impl LockedRoot {
    fn acquire(path: &Path) -> io::Result<Self> {
        let directory = private_directory(path)?;
        let lock = File::from(filesystem::openat(
            &directory,
            LOCK_NAME,
            FILE_FLAGS | OFlags::RDWR | OFlags::CREATE,
            Mode::RUSR | Mode::WUSR,
        )?);
        let metadata = private_file(&lock)?;
        if metadata.len() != 0 {
            return Err(invalid());
        }
        filesystem::flock(&lock, FlockOperation::NonBlockingLockExclusive)?;
        let locked = Self { directory, lock };
        let current = File::from(filesystem::openat(
            &locked.directory,
            LOCK_NAME,
            READ_FLAGS,
            Mode::empty(),
        )?);
        if !same_file(&metadata, &private_file(&current)?) {
            return Err(invalid());
        }
        Ok(locked)
    }

    fn inventory(&self) -> io::Result<Inventory> {
        let mut inventory = Inventory {
            entries: Vec::new(),
            bytes: 0,
        };
        for entry in Dir::read_from(&self.directory)? {
            let directory_entry = entry?;
            let name = directory_entry.file_name();
            if name == c"." || name == c".." || name == LOCK_NAME {
                continue;
            }
            let pending = pending_name(name);
            if !pending && !png_name(name) {
                // Unknown paths are not ours to delete or exclude from accounting.
                return Err(invalid());
            }
            let file = File::from(filesystem::openat(
                &self.directory,
                name,
                READ_FLAGS,
                Mode::empty(),
            )?);
            let metadata = private_file(&file)?;
            if pending {
                // The lock spans every writer's lifetime, so no pending file is live.
                self.remove(name, &metadata)?;
                continue;
            }
            inventory.bytes = inventory
                .bytes
                .checked_add(metadata.len())
                .ok_or_else(invalid)?;
            if inventory.entries.len() > MAX_ENTRIES {
                return Err(invalid());
            }
            inventory.entries.push(Entry {
                directory_entry,
                metadata,
            });
        }
        Ok(inventory)
    }

    fn trim(
        &self,
        inventory: &mut Inventory,
        limits: Limits,
        incoming_bytes: u64,
        incoming_entries: usize,
    ) -> io::Result<()> {
        let projected_bytes = inventory
            .bytes
            .checked_add(incoming_bytes)
            .ok_or_else(invalid)?;
        let projected_entries = inventory
            .entries
            .len()
            .checked_add(incoming_entries)
            .ok_or_else(invalid)?;
        if projected_bytes <= limits.bytes && projected_entries <= limits.entries {
            return Ok(());
        }
        let target_bytes = (limits.bytes - limits.bytes.div_ceil(4)).max(incoming_bytes);
        let target_entries = (limits.entries - limits.entries.div_ceil(4)).max(incoming_entries);
        inventory.entries.sort_unstable_by(|left, right| {
            left.metadata
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .cmp(&right.metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                .then_with(|| left.name().cmp(right.name()))
        });
        let mut removed = 0;
        while inventory.bytes + incoming_bytes > target_bytes
            || inventory.entries.len() - removed + incoming_entries > target_entries
        {
            let entry = inventory.entries.get(removed).ok_or_else(invalid)?;
            self.remove(entry.name(), &entry.metadata)?;
            inventory.bytes -= entry.metadata.len();
            removed += 1;
        }
        inventory.entries.drain(..removed);
        Ok(())
    }

    fn remove(&self, name: &CStr, expected: &Metadata) -> io::Result<()> {
        let file = File::from(filesystem::openat(
            &self.directory,
            name,
            READ_FLAGS,
            Mode::empty(),
        )?);
        if !same_file(expected, &private_file(&file)?) {
            return Err(invalid());
        }
        filesystem::unlinkat(&self.directory, name, AtFlags::empty())?;
        Ok(())
    }
}

impl Drop for LockedRoot {
    fn drop(&mut self) {
        // Closing alone would retain the lease in a fork-inherited open description.
        let _ = filesystem::flock(&self.lock, FlockOperation::Unlock);
    }
}

struct Inventory {
    entries: Vec<Entry>,
    bytes: u64,
}

struct Entry {
    directory_entry: DirEntry,
    metadata: Metadata,
}

impl Entry {
    fn name(&self) -> &CStr {
        self.directory_entry.file_name()
    }
}

struct PendingFile<'a> {
    directory: &'a File,
    name: CString,
    file: tempfile::NamedTempFile,
    published: bool,
}

impl<'a> PendingFile<'a> {
    fn new(directory: &'a File, root: &Path) -> io::Result<Self> {
        // Keep tempfile's atomic exclusive creation and randomized names, but perform
        // all operations relative to the pinned directory, even if its path moves.
        let file = tempfile::Builder::new()
            .prefix(PENDING_PREFIX)
            .suffix(PENDING_SUFFIX)
            .rand_bytes(PENDING_RANDOM_LEN)
            .disable_cleanup(true)
            .make_in(root, |path| {
                let name = c_name(path.file_name().ok_or_else(invalid)?)?;
                Ok(File::from(filesystem::openat(
                    directory,
                    &name,
                    FILE_FLAGS | OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
                    Mode::RUSR | Mode::WUSR,
                )?))
            })?;
        let name = c_name(file.path().file_name().ok_or_else(invalid)?)?;
        Ok(Self {
            directory,
            name,
            file,
            published: false,
        })
    }
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // TempPath cleanup is disabled because following its parent path could
        // unlink an unrelated file after a directory rename or symlink replacement.
        if let Ok(file) = filesystem::openat(self.directory, &self.name, READ_FLAGS, Mode::empty())
            .map(File::from)
            && let (Ok(current), Ok(expected)) =
                (private_file(&file), self.file.as_file().metadata())
            && same_file(&current, &expected)
        {
            let _ = filesystem::unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}

fn private_directory(path: &Path) -> io::Result<File> {
    if !path
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(invalid());
    }
    let start = if path.is_absolute() { c"/" } else { c"." };
    let mut directory = File::from(filesystem::open(
        start,
        READ_FLAGS | OFlags::DIRECTORY,
        Mode::empty(),
    )?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            let next = match filesystem::openat(
                &directory,
                name,
                READ_FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            ) {
                Ok(next) => next,
                Err(Errno::NOENT) => {
                    match filesystem::mkdirat(&directory, name, Mode::RWXU) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    filesystem::openat(
                        &directory,
                        name,
                        READ_FLAGS | OFlags::DIRECTORY,
                        Mode::empty(),
                    )?
                }
                Err(error) => return Err(error.into()),
            };
            directory = File::from(next);
        }
    }
    let metadata = directory.metadata()?;
    if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(directory)
}

fn private_file(file: &File) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(invalid());
    }
    Ok(metadata)
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| invalid())
}

struct KeyName([u8; 69]);

impl KeyName {
    fn as_c_str(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.0).expect("hex cache names contain no NUL")
    }
}

fn key_name(key: &[u8; 32]) -> KeyName {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = [0; 69];
    for (index, byte) in key.iter().enumerate() {
        name[index * 2] = HEX[usize::from(byte >> 4)];
        name[index * 2 + 1] = HEX[usize::from(byte & 15)];
    }
    name[64..68].copy_from_slice(b".png");
    KeyName(name)
}

fn png_name(name: &CStr) -> bool {
    let bytes = name.to_bytes();
    bytes.len() == 68
        && bytes.ends_with(b".png")
        && bytes[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn pending_name(name: &CStr) -> bool {
    let bytes = name.to_bytes();
    bytes.len() == PENDING_PREFIX.len() + PENDING_RANDOM_LEN + PENDING_SUFFIX.len()
        && bytes.starts_with(PENDING_PREFIX.as_bytes())
        && bytes.ends_with(PENDING_SUFFIX.as_bytes())
        && bytes[PENDING_PREFIX.len()..bytes.len() - PENDING_SUFFIX.len()]
            .iter()
            .all(u8::is_ascii_alphanumeric)
}

fn invalid() -> io::Error {
    io::Error::other("unsafe or incompatible chrome cache directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use std::process::{Command, Stdio};

    fn private_fixture() -> tempfile::TempDir {
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let root = tempfile::tempdir_in(parent).unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn cache_path(root: &Path, key: u8) -> PathBuf {
        root.join(OsStr::from_bytes(
            key_name(&[key; 32]).as_c_str().to_bytes(),
        ))
    }

    fn stored(root: &Path) -> (usize, u64) {
        let mut count = 0;
        let mut bytes = 0;
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().as_bytes() != LOCK_NAME.to_bytes() {
                count += 1;
                bytes += entry.metadata().unwrap().len();
            }
        }
        (count, bytes)
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    #[test]
    fn shared_bytes_read_bounds_and_removal() {
        let fixture = private_fixture();
        let root = fixture.path().join("cache");
        let mut first = DiskCache::new(root.clone());
        assert!(!root.exists());
        assert!(first.put(&[1; 32], b"first PNG"));
        let mut second = DiskCache::new(root.clone());
        assert_eq!(second.get(&[1; 32], 8), None);
        assert_eq!(second.get(&[1; 32], 9), Some(b"first PNG".to_vec()));
        assert!(second.put(&[1; 32], b"replacement"));
        assert_eq!(first.get(&[1; 32], 11), Some(b"replacement".to_vec()));
        first.remove(&[1; 32]);
        assert_eq!(second.get(&[1; 32], 11), None);
        assert_eq!(stored(&root), (0, 0));
        assert_eq!(fs::metadata(root).unwrap().mode() & 0o077, 0);
    }

    #[test]
    fn count_pressure_trims_least_recently_used_to_three_quarters() {
        let root = private_fixture();
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 100, 4);
        for key in 1..=4 {
            assert!(cache.put(&[key; 32], &[key]));
            File::open(cache_path(root.path(), key))
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(key)))
                .unwrap();
        }
        assert_eq!(cache.get(&[1; 32], 1), Some(vec![1]));
        assert!(cache.put(&[5; 32], &[5]));
        assert_eq!(stored(root.path()), (3, 3));
        assert_eq!(cache.get(&[1; 32], 1), Some(vec![1]));
        assert_eq!(cache.get(&[2; 32], 1), None);
        assert_eq!(cache.get(&[3; 32], 1), None);
        assert_eq!(cache.get(&[4; 32], 1), Some(vec![4]));
    }

    #[test]
    fn recent_reads_do_not_promote_entries_before_the_hourly_touch() {
        let root = private_fixture();
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 100, 4);
        let now = SystemTime::now();
        for key in 1..=4 {
            assert!(cache.put(&[key; 32], &[key]));
            File::open(cache_path(root.path(), key))
                .unwrap()
                .set_modified(now - Duration::from_secs(300 * u64::from(5 - key)))
                .unwrap();
        }
        assert_eq!(cache.get(&[1; 32], 1), Some(vec![1]));
        assert!(cache.put(&[5; 32], &[5]));
        assert_eq!(cache.get(&[1; 32], 1), None);
        assert_eq!(cache.get(&[2; 32], 1), None);
        assert_eq!(cache.get(&[3; 32], 1), Some(vec![3]));
        assert_eq!(cache.get(&[4; 32], 1), Some(vec![4]));
        assert_eq!(stored(root.path()), (3, 3));
    }

    #[test]
    fn contention_bypasses_without_modifying_existing_entries() {
        let root = private_fixture();
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 16, 4);
        assert!(cache.put(&[1; 32], b"original"));
        let lease = LockedRoot::acquire(root.path()).unwrap();
        assert_eq!(cache.get(&[1; 32], 16), None);
        assert!(!cache.put(&[1; 32], b"replacement"));
        cache.remove(&[1; 32]);
        drop(lease);
        assert_eq!(cache.get(&[1; 32], 16), Some(b"original".to_vec()));
    }

    #[test]
    fn byte_pressure_counts_pending_replacement_and_allows_large_entries() {
        let root = private_fixture();
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 16, 8);
        for key in 1..=3 {
            assert!(cache.put(&[key; 32], &[key; 4]));
            File::open(cache_path(root.path(), key))
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(key)))
                .unwrap();
        }
        assert!(cache.put(&[3; 32], &[9; 8]));
        assert_eq!(stored(root.path()), (1, 8));
        assert_eq!(cache.get(&[3; 32], 8), Some(vec![9; 8]));
        assert!(!cache.put(&[4; 32], &[4; 17]));
        assert_eq!(cache.get(&[3; 32], 8), Some(vec![9; 8]));
        assert!(cache.put(&[4; 32], &[4; 16]));
        assert_eq!(stored(root.path()), (1, 16));
        assert_eq!(cache.get(&[4; 32], 16), Some(vec![4; 16]));
    }

    #[test]
    fn orphan_pending_bytes_are_reclaimed_before_admission() {
        let root = private_fixture();
        write_private(&root.path().join(".pending-1234567890abcdef.tmp"), &[0; 8]);
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 8, 2);
        assert!(cache.put(&[1; 32], &[1; 8]));
        assert_eq!(stored(root.path()), (1, 8));
        assert_eq!(cache.get(&[1; 32], 8), Some(vec![1; 8]));
    }

    #[test]
    fn unsafe_or_unexpected_paths_never_follow_or_delete_unrelated_content() {
        let fixture = private_fixture();
        let root = fixture.path().join("cache");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let outside = fixture.path().join("unrelated");
        write_private(&outside, b"private source");
        symlink(&outside, cache_path(&root, 1)).unwrap();
        let mut cache = DiskCache::new(root.clone());
        assert_eq!(cache.get(&[1; 32], 100), None);
        assert!(!cache.put(&[1; 32], b"replacement"));
        cache.remove(&[1; 32]);
        assert!(
            fs::symlink_metadata(cache_path(&root, 1))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read(&outside).unwrap(), b"private source");
        fs::remove_file(cache_path(&root, 1)).unwrap();
        fs::create_dir(root.join("unrelated-directory")).unwrap();
        assert!(!cache.put(&[2; 32], b"new PNG"));
        assert!(root.join("unrelated-directory").is_dir());
        fs::remove_dir(root.join("unrelated-directory")).unwrap();
        write_private(&root.join("unrelated-file"), b"not cache-owned");
        assert!(!cache.put(&[2; 32], b"new PNG"));
        assert_eq!(
            fs::read(root.join("unrelated-file")).unwrap(),
            b"not cache-owned"
        );
        let alias = fixture.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(!DiskCache::new(alias.join("nested")).put(&[1; 32], b"new PNG"));
        assert!(!root.join("nested").exists());
    }

    #[test]
    fn corrupt_or_oversized_reads_do_not_escape_the_callers_bound() {
        let root = private_fixture();
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 16, 4);
        assert!(cache.put(&[1; 32], b"PNG bytes"));
        fs::OpenOptions::new()
            .write(true)
            .open(cache_path(root.path(), 1))
            .unwrap()
            .set_len(10)
            .unwrap();
        assert_eq!(cache.get(&[1; 32], 9), None);
        assert!(cache.put(&[1; 32], b"valid again"));
        assert_eq!(cache.get(&[1; 32], 11), Some(b"valid again".to_vec()));
    }

    #[test]
    #[ignore = "private subprocess fixture; run through the concurrent-process wrapper"]
    fn process_worker() {
        let mut worker = [0];
        io::stdin().read_exact(&mut worker).unwrap();
        let worker = worker[0];
        let mut cache = DiskCache::with_limits(std::env::current_dir().unwrap(), 128, 8);
        for turn in 0..64_u8 {
            let key = [turn % 12; 32];
            let bytes = [worker; 24];
            let _ = cache.put(&key, &bytes);
            if let Some(read) = cache.get(&key, 24) {
                assert_eq!(read.len(), 24);
                assert!(read.iter().all(|byte| *byte == read[0]));
            }
        }
    }

    #[test]
    fn concurrent_processes_never_publish_partial_bytes_or_exceed_shared_limits() {
        let root = private_fixture();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for worker in 1..=4_u8 {
            let mut child = Command::new(&executable)
                .arg("--ignored")
                .arg("--exact")
                .arg("graphics_cache::tests::process_worker")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .current_dir(root.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&[worker]).unwrap();
            children.push(child);
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let mut cache = DiskCache::with_limits(root.path().to_path_buf(), 128, 8);
        assert!(cache.put(&[99; 32], &[99; 24]));
        let (entries, bytes) = stored(root.path());
        assert!(entries <= 8);
        assert!(bytes <= 128);
        for key in 0..12 {
            if let Some(read) = cache.get(&[key; 32], 24) {
                assert_eq!(read.len(), 24);
                assert!(read.iter().all(|byte| *byte == read[0]));
            }
        }
    }
}
