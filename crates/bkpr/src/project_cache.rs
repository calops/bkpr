//! Last-known committed Git/forge metadata, isolated by canonical working directory.
use crate::project::ProjectName;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use bkpr::{
    BranchChanges,
    ci_contract::{CiFetchState, CiSnapshot},
};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) struct Cache {
    root: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Record {
    version: u32,
    cwd: PathBuf,
    repository_id: String,
    branch: Option<String>,
    head: String,
    git_refs: Option<String>,
    ci: Option<CiSnapshot>,
    branch_changes: Option<BranchChanges>,
}

#[derive(Serialize)]
struct StoredRecord<'a> {
    version: u32,
    cwd: &'a Path,
    repository_id: &'a str,
    branch: &'a Option<String>,
    head: &'a str,
    git_refs: &'a Option<String>,
    ci: Option<&'a CiSnapshot>,
    branch_changes: &'a Option<BranchChanges>,
}

pub(crate) struct CachedProject {
    pub ci: Option<CiSnapshot>,
    pub branch_changes: Option<BranchChanges>,
}

impl Cache {
    pub(crate) fn new() -> Self {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|home| home.join(".local/state"))
            });
        Self {
            root: state.map(|state| state.join("bkpr/projects")),
        }
    }

    fn directory(&self, cwd: &Path) -> Option<PathBuf> {
        // The record also checks the complete path, so a hash collision is a miss.
        let hash = cwd
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        Some(self.root.as_ref()?.join(format!("{hash:016x}")))
    }

    pub(crate) fn load(&self, cwd: &str, project: &ProjectName) -> Option<CachedProject> {
        let cwd = fs::canonicalize(cwd).ok()?;
        let directory = self.directory(&cwd)?;
        let record = read(&directory.join("state.json"))?;
        let head = committed_head(&cwd, project)?;
        if record.version != VERSION
            || record.cwd != cwd
            || Some(&record.repository_id) != project.repository_id.as_ref()
            || record.git_refs != project.git_refs
            || record.branch != project.branch
            || record.head != head
            || !matches_source(record.ci.as_ref(), project)
        {
            return None;
        }
        // Without a forge target, local configuration chooses the diff base.
        if record
            .ci
            .as_ref()
            .and_then(|ci| ci.change_request.as_ref())
            .and_then(|request| request.base.as_ref())
            .is_none()
            && record.branch_changes != project.branch_changes
        {
            return None;
        }
        Some(CachedProject {
            ci: record.ci,
            branch_changes: record.branch_changes,
        })
    }

    pub(crate) fn store(&self, cwd: &str, project: &ProjectName) -> Result<()> {
        let ci = project.ci.as_ref();
        if !matches_source(ci, project) {
            return Ok(());
        }
        let Some(repository_id) = project.repository_id.as_ref() else {
            return Ok(());
        };
        let Some(head) = committed_head(Path::new(cwd), project) else {
            return Ok(());
        };
        let cwd = fs::canonicalize(cwd)?;
        let Some(directory) = self.directory(&cwd) else {
            return Ok(());
        };
        crate::launcher::private_directory(&directory)?;
        let _lock = crate::launcher::startup_lock(&directory)?;
        let path = directory.join("state.json");
        if let Some(previous) = read(&path)
            && previous.cwd == cwd
            && previous.repository_id == *repository_id
            && previous.head == head
            && previous.branch == project.branch
            && let (Some(previous), Some(ci)) = (previous.ci.as_ref(), ci)
            && previous.updated_at > ci.updated_at
        {
            return Ok(());
        }
        let record = StoredRecord {
            version: VERSION,
            cwd: &cwd,
            repository_id,
            branch: &project.branch,
            head: &head,
            git_refs: &project.git_refs,
            ci,
            branch_changes: &project.branch_changes,
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer(temporary.as_file_mut(), &record)?;
        temporary.as_file_mut().flush()?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path)?;
        File::open(&directory)?.sync_all()?;
        Ok(())
    }
}

fn committed_head<'a>(cwd: &Path, project: &'a ProjectName) -> Option<std::borrow::Cow<'a, str>> {
    match &project.source {
        Some(source) => Some(std::borrow::Cow::Borrowed(&source.sha)),
        None => crate::project::git_text(cwd, &["rev-parse", "--verify", "HEAD"])
            .map(std::borrow::Cow::Owned),
    }
}

fn matches_source(ci: Option<&CiSnapshot>, project: &ProjectName) -> bool {
    match (ci, project.source.as_ref()) {
        (Some(ci), Some(source)) => {
            ci.fetch_state != CiFetchState::Loading
                && !(ci.commit_missing && project.published)
                && ci.repository == source.repository
                && ci.head_sha == source.sha
                && ci.branch == source.branch
        }
        (None, None) => true,
        _ => false,
    }
}

fn read(path: &Path) -> Option<Record> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > MAX_BYTES
    {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::CiSource;
    use serde_json::json;

    fn project() -> ProjectName {
        let ci: CiSnapshot = serde_json::from_value(json!({
            "repository":{"provider":"github","host":"github.com","path":"team/repo","web_url":"https://github.com/team/repo"},
            "head_sha":"head", "branch":"feature", "status":"passed", "fetch_state":"ready",
            "checks":[{"name":"test","status":"passed","url":null}],
            "change_request":null,"updated_at":123,"stale":false,"error":null,"truncated":false
        })).unwrap();
        ProjectName {
            name: "repo".into(),
            repository: true,
            branch: ci.branch.clone(),
            repository_id: Some("/repository/.git".into()),
            branch_changes: Some(BranchChanges {
                base_ref: "refs/heads/main".into(),
                merge_base: "base".into(),
                total: bkpr::DiffStat {
                    added: 4,
                    deleted: 2,
                    binary_files: 0,
                },
            }),
            source: Some(CiSource {
                repository: ci.repository.clone(),
                sha: ci.head_sha.clone(),
                branch: ci.branch.clone(),
            }),
            ci: Some(ci),
            git_refs: Some("base refs/heads/main\nhead refs/heads/feature".into()),
            published: true,
        }
    }

    #[test]
    fn unchanged_identity_restores_metadata_but_head_branch_refs_and_base_changes_do_not() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        fs::create_dir(&cwd).unwrap();
        let cache = Cache {
            root: Some(root.path().join("state")),
        };
        let original = project();
        let cwd = cwd.to_str().unwrap();
        cache.store(cwd, &original).unwrap();
        let restored = cache.load(cwd, &original).unwrap();
        assert_eq!(restored.ci, original.ci);
        assert_eq!(restored.branch_changes, original.branch_changes);
        for change in 0..5 {
            let mut changed = original.clone();
            match change {
                0 => changed.source.as_mut().unwrap().sha = "new-head".into(),
                1 => changed.source.as_mut().unwrap().branch = Some("other".into()),
                2 => changed.repository_id = Some("/other/.git".into()),
                3 => changed.git_refs = Some("advanced refs/heads/main".into()),
                _ => changed.branch_changes.as_mut().unwrap().merge_base = "new-base".into(),
            }
            assert!(cache.load(cwd, &changed).is_none(), "case {change}");
        }
    }

    #[test]
    fn directories_are_isolated_and_corrupt_or_loading_data_cannot_replace_settled_metadata() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("one");
        let second = root.path().join("two");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let cache = Cache {
            root: Some(root.path().join("state")),
        };
        let original = project();
        cache.store(first.to_str().unwrap(), &original).unwrap();
        assert!(cache.load(second.to_str().unwrap(), &original).is_none());
        let mut loading = original.clone();
        loading.ci.as_mut().unwrap().fetch_state = CiFetchState::Loading;
        loading.ci.as_mut().unwrap().change_request = None;
        cache.store(first.to_str().unwrap(), &loading).unwrap();
        assert_eq!(
            cache.load(first.to_str().unwrap(), &original).unwrap().ci,
            original.ci
        );
        let directory = cache.directory(&fs::canonicalize(&first).unwrap()).unwrap();
        fs::write(directory.join("state.json"), b"{partial").unwrap();
        assert!(cache.load(first.to_str().unwrap(), &project()).is_none());
    }

    #[test]
    fn git_only_repositories_restore_totals_and_reject_a_later_head() {
        let repository = crate::project::tests::Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.git(&["commit", "--allow-empty", "-m", "main"]);
        repository.git(&["switch", "-c", "feature"]);
        fs::write(repository.0.join("implementation.rs"), "one\ntwo\n").unwrap();
        repository.commit_all();
        let state = tempfile::tempdir().unwrap();
        let cache = Cache {
            root: Some(state.path().join("projects")),
        };
        let original = crate::project::discover(&repository.0).unwrap();
        assert!(original.source.is_none());
        cache
            .store(repository.0.to_str().unwrap(), &original)
            .unwrap();
        let restored = cache
            .load(repository.0.to_str().unwrap(), &original)
            .unwrap();
        assert!(restored.ci.is_none());
        assert_eq!(restored.branch_changes.unwrap().total.added, 2);
        fs::write(repository.0.join("implementation.rs"), "three\n").unwrap();
        repository.commit_all();
        let next = crate::project::discover(&repository.0).unwrap();
        assert!(cache.load(repository.0.to_str().unwrap(), &next).is_none());
    }

    #[test]
    fn publication_cannot_restore_an_earlier_missing_commit_as_permanently_idle() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        fs::create_dir(&cwd).unwrap();
        let cache = Cache {
            root: Some(root.path().join("state")),
        };
        let mut unpublished = project();
        unpublished.published = false;
        let ci = unpublished.ci.as_mut().unwrap();
        ci.commit_missing = true;
        ci.fetch_state = CiFetchState::Unavailable;
        ci.status = bkpr::ci_contract::CiStatus::Unknown;
        ci.checks.clear();
        ci.error = Some("Commit not available".into());
        cache.store(cwd.to_str().unwrap(), &unpublished).unwrap();
        assert!(cache.load(cwd.to_str().unwrap(), &unpublished).is_some());
        let mut published = unpublished.clone();
        published.published = true;
        assert!(cache.load(cwd.to_str().unwrap(), &published).is_none());
    }
}
