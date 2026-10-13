use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectName {
    pub name: String,
    pub repository: bool,
    pub branch: Option<String>,
    pub repository_id: Option<String>,
    pub branch_changes: Option<shephrd::BranchChanges>,
    pub ci: Option<shephrd::ci_contract::CiSnapshot>,
    #[serde(skip)]
    pub source: Option<crate::ci::CiSource>,
}

pub(crate) fn discover(cwd: &Path) -> Result<ProjectName> {
    Ok(discover_watched(cwd)?.0)
}

/// The git-dir owns HEAD, while the common directory owns refs in linked worktrees.
pub(crate) fn discover_watched(cwd: &Path) -> Result<(ProjectName, Vec<std::path::PathBuf>)> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        bail!("project cwd must be an existing absolute directory");
    }
    let mut directories: Vec<_> = cwd.ancestors().map(Path::to_path_buf).collect();
    // Git init can still be incomplete when its parent emits the creation event.
    for ancestor in cwd.ancestors() {
        let metadata = ancestor.join(".git");
        if metadata.is_dir() {
            directories.push(metadata);
        }
    }
    let output = git_command(cwd)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--absolute-git-dir",
        ])
        .output();
    if let Ok(output) = output
        && output.status.success()
        && let Ok(text) = std::str::from_utf8(&output.stdout)
    {
        let lines: Vec<_> = text.lines().collect();
        if let [root, common, git_dir] = lines.as_slice()
            && let Some(name) = repository_name(format!("{root}\n{common}\n").as_bytes())
        {
            for directory in [Path::new(git_dir), Path::new(common)] {
                directories.push(directory.to_path_buf());
                if let Some(parent) = directory.parent() {
                    directories.push(parent.to_path_buf());
                }
            }
            // Ref parents cover local stacked-branch bases as well as remote CI refs.
            // The common directory also covers atomic packed-refs/config replacement.
            directories.push(Path::new(common).join("refs"));
            directories.push(Path::new(common).join("refs/heads"));
            directories.push(Path::new(common).join("refs/remotes"));
            let refs = git_text(cwd, &["for-each-ref", "--format=%(refname)"]);
            if let Some(refs) = &refs {
                for reference in refs.lines() {
                    reference_directories(Path::new(common), reference, &mut directories);
                }
            }
            // Keep empty ref directories too: creating a ref inside one produces no
            // event on its parent. These are metadata-only, never source-tree watches.
            let mut pending = vec![Path::new(common).join("refs")];
            while let Some(directory) = pending.pop() {
                if let Ok(entries) = std::fs::read_dir(&directory) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                            pending.push(entry.path());
                        }
                    }
                }
                directories.push(directory);
            }
            let symbolic = git_text(cwd, &["symbolic-ref", "--quiet", "HEAD"]);
            let branch = match symbolic.as_deref() {
                Some(reference) => {
                    reference_directories(Path::new(common), reference, &mut directories);
                    reference.strip_prefix("refs/heads/").map(str::to_owned)
                }
                None => git_text(cwd, &["rev-parse", "--short=7", "HEAD"])
                    .map(|sha| format!("detached @ {sha}")),
            };
            let branch_changes = branch_changes(
                cwd,
                symbolic
                    .as_deref()
                    .and_then(|name| name.strip_prefix("refs/heads/")),
                refs.as_deref().unwrap_or_default(),
                Path::new(common),
                &mut directories,
            );
            let repository_id = std::fs::canonicalize(common)
                .ok()
                .and_then(|path| path.to_str().map(str::to_owned));
            directories.sort_unstable();
            directories.dedup();
            let source = crate::ci::discover(cwd, branch.as_deref());
            return Ok((
                ProjectName {
                    name,
                    repository: true,
                    branch,
                    repository_id,
                    branch_changes,
                    ci: None,
                    source,
                },
                directories,
            ));
        }
    }
    Ok((
        directory_name(cwd).context("project directory name is not UTF-8")?,
        directories,
    ))
}

fn reference_directories(
    common: &Path,
    reference: &str,
    directories: &mut Vec<std::path::PathBuf>,
) {
    // Only metadata refs, never a configured revision's arbitrary filesystem path.
    if !reference.starts_with("refs/")
        || Path::new(reference)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return;
    }
    let path = common.join(reference);
    if let Some(parent) = path.parent() {
        directories.extend(
            parent
                .ancestors()
                .take_while(|directory| directory.starts_with(common))
                .map(Path::to_path_buf),
        );
    }
}

fn resolve_commit(cwd: &Path, revision: &str) -> Option<String> {
    git_text(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )
}

fn branch_changes(
    cwd: &Path,
    branch: Option<&str>,
    refs: &str,
    common: &Path,
    directories: &mut Vec<std::path::PathBuf>,
) -> Option<shephrd::BranchChanges> {
    let configured = branch.and_then(|branch| {
        git_text(
            cwd,
            &["config", "--get", &format!("branch.{branch}.gh-merge-base")],
        )
    });
    let (base_ref, base) = if let Some(base_ref) = configured {
        // An explicit stacked-branch target is authoritative, including when unresolved.
        // Subscribe its potential loose ref before attempting resolution.
        if base_ref.is_empty() {
            return None;
        }
        if base_ref.starts_with("refs/") {
            reference_directories(common, &base_ref, directories);
        } else {
            reference_directories(common, &format!("refs/heads/{base_ref}"), directories);
        }
        let base = resolve_commit(cwd, &base_ref)?;
        (base_ref, base)
    } else {
        let remote = branch
            .and_then(|branch| {
                git_text(
                    cwd,
                    &["config", "--get", &format!("branch.{branch}.remote")],
                )
            })
            .filter(|remote| remote != ".");
        let mut remotes = Vec::new();
        if let Some(remote) = remote {
            remotes.push(remote);
        }
        if !remotes.iter().any(|remote| remote == "origin") {
            remotes.push("origin".to_owned());
        }
        let preferred = remotes.len();
        for reference in refs.lines() {
            if let Some(remote) = reference
                .strip_prefix("refs/remotes/")
                .and_then(|reference| reference.split_once('/').map(|(remote, _)| remote))
                && !remotes.iter().any(|name| name == remote)
            {
                remotes.push(remote.to_owned());
            }
        }
        remotes[preferred..].sort_unstable();
        let mut candidates = Vec::new();
        for remote in &remotes[..preferred] {
            candidates.push(format!("refs/remotes/{remote}/HEAD"));
        }
        for remote in &remotes {
            for name in ["main", "master"] {
                candidates.push(format!("refs/remotes/{remote}/{name}"));
            }
        }
        candidates.extend(["refs/heads/main".to_owned(), "refs/heads/master".to_owned()]);
        candidates
            .into_iter()
            .filter(|reference| refs.lines().any(|name| name == reference))
            .find_map(|reference| {
                resolve_commit(cwd, &reference).map(|commit| (reference, commit))
            })?
    };
    let symbolic_base = git_text(
        cwd,
        &[
            "rev-parse",
            "--symbolic-full-name",
            "--verify",
            "--end-of-options",
            &base_ref,
        ],
    );
    if let Some(reference) = &symbolic_base {
        reference_directories(common, reference, directories);
    }
    // Resolve both endpoints to immutable commit IDs before merge-base and diff.
    let head = resolve_commit(cwd, "HEAD")?;
    let merge_base = git_text(cwd, &["merge-base", &base, &head])?;
    let output = git_command(cwd)
        .args([
            "diff",
            "--numstat",
            "-z",
            "--find-renames=50%",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            &merge_base,
            &head,
            "--",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let total = diff_stats(&output.stdout)?;
    Some(shephrd::BranchChanges {
        base_ref: symbolic_base
            .filter(|reference| !reference.is_empty())
            .unwrap_or(base_ref),
        merge_base,
        total,
    })
}

fn diff_stats(output: &[u8]) -> Option<shephrd::DiffStat> {
    let mut total = shephrd::DiffStat::default();
    let mut records = output.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            // The only empty record outside a rename's paths is the final terminator.
            if records.next().is_none() {
                break;
            }
            return None;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next()?;
        let deleted = fields.next()?;
        let path = fields.next()?;
        if path.is_empty() {
            // -z renames include two additional NUL-delimited paths.
            let old = records.next()?;
            let new = records.next()?;
            if old.is_empty() || new.is_empty() {
                return None;
            }
        }
        let stat = if added == b"-" && deleted == b"-" {
            shephrd::DiffStat {
                binary_files: 1,
                ..shephrd::DiffStat::default()
            }
        } else {
            shephrd::DiffStat {
                added: std::str::from_utf8(added).ok()?.parse().ok()?,
                deleted: std::str::from_utf8(deleted).ok()?.parse().ok()?,
                binary_files: 0,
            }
        };
        total.added = total.added.checked_add(stat.added)?;
        total.deleted = total.deleted.checked_add(stat.deleted)?;
        total.binary_files = total.binary_files.checked_add(stat.binary_files)?;
    }
    Some(total)
}

pub(crate) fn git_text(cwd: &Path, arguments: &[&str]) -> Option<String> {
    let output = git_command(cwd).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        std::str::from_utf8(&output.stdout)
            .ok()?
            .trim_end_matches('\n')
            .to_owned(),
    )
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd);
    // Metadata belongs to the reported cwd, not a repository override inherited by Zellij.
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        command.env_remove(name);
    }
    command
}

pub(crate) fn directory_name(cwd: &Path) -> Option<ProjectName> {
    Some(ProjectName {
        name: cwd
            .file_name()
            .unwrap_or(cwd.as_os_str())
            .to_str()?
            .to_owned(),
        repository: false,
        branch: None,
        repository_id: None,
        branch_changes: None,
        ci: None,
        source: None,
    })
}

pub(crate) fn repository_name(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let mut lines = text.lines().rev();
    let common = Path::new(lines.next()?);
    let root = Path::new(lines.next()?);
    if !root.is_absolute() || !common.is_absolute() {
        return None;
    }
    let repository = if common.file_name().is_some_and(|name| name == ".git") {
        common.parent()?
    } else {
        root
    };
    repository.file_name()?.to_str().map(str::to_owned)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Repository(pub PathBuf);

    impl Repository {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "shephrd-project-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        pub(crate) fn git(&self, arguments: &[&str]) {
            self.git_at(&self.0, arguments);
        }

        pub(crate) fn git_at(&self, cwd: &Path, arguments: &[&str]) {
            let output = git_command(cwd)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .args([
                    "-c",
                    "user.name=Shephrd Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "tag.gpgsign=false",
                ])
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        pub(crate) fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        pub(crate) fn commit_all(&self) {
            self.git(&["add", "--all"]);
            self.git(&["commit", "-m", "fixture"]);
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn branches_follow_git_head_and_preserve_unborn_and_detached_states() {
        let repository = Repository::new();
        assert_eq!(discover(&repository.0).unwrap().branch, None);
        repository.git(&["init", "-b", "initial"]);
        assert_eq!(
            discover(&repository.0).unwrap().branch.as_deref(),
            Some("initial")
        );
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        repository.git(&["switch", "-c", "feature/nested"]);
        assert_eq!(
            discover(&repository.0).unwrap().branch.as_deref(),
            Some("feature/nested")
        );
        repository.git(&["checkout", "--detach"]);
        let sha = git_text(&repository.0, &["rev-parse", "--short=7", "HEAD"]).unwrap();
        assert_eq!(
            discover(&repository.0).unwrap().branch,
            Some(format!("detached @ {sha}"))
        );
    }

    #[test]
    fn linked_worktrees_use_their_own_head_and_shared_repository_name() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        let worktree = repository.0.join("linked");
        repository.git(&[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            worktree.to_str().unwrap(),
        ]);
        let root = discover(&repository.0).unwrap();
        let (linked, directories) = discover_watched(&worktree).unwrap();
        assert_eq!(root.branch.as_deref(), Some("main"));
        assert_eq!(linked.branch.as_deref(), Some("linked-branch"));
        assert_eq!(linked.name, root.name);
        assert_eq!(linked.repository_id, root.repository_id);
        assert!(root.repository_id.is_some());
        assert!(
            directories
                .iter()
                .any(|directory| directory.join("HEAD").is_file()
                    && directory.starts_with(repository.0.join(".git/worktrees")))
        );
    }

    #[test]
    fn branch_changes_cover_all_files_at_the_committed_merge_base() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.write("src/logic.rs", "keep\nremove\n");
        repository.write("old.rs", "old\n");
        repository.write("deleted.rs", "deleted\n");
        repository.write("asset.bin", [0, 1]);
        repository.commit_all();
        let initial = resolve_commit(&repository.0, "HEAD").unwrap();
        repository.git(&["switch", "-c", "feature"]);
        repository.write("src/logic.rs", "keep\nnew\n#[test] fn inline_test() {}\n");
        repository.git(&["mv", "old.rs", "src/renamed.rs"]);
        repository.git(&["rm", "deleted.rs"]);
        repository.write("src/café notes.rs", "unicode\n");
        repository.write("src/line\nbreak.rs", "newline filename\n");
        repository.write("asset.bin", [0, 2]);
        repository.write("docs/image.bin", [0, 3]);
        for path in ["tests/unit.rs", "docs/guide.md", "README.md"] {
            repository.write(path, "content\n");
        }
        repository.commit_all();
        repository.git(&["switch", "main"]);
        repository.write("target-only.rs", "target tip is not the comparison tree\n");
        repository.commit_all();
        repository.git(&["switch", "feature"]);
        // Neither tracked worktree edits nor untracked files contribute.
        repository.write("src/logic.rs", "uncommitted\n");
        repository.write("untracked.rs", "uncommitted\n");
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.base_ref, "refs/heads/main");
        assert_eq!(changes.merge_base, initial);
        assert_eq!(
            changes.total,
            shephrd::DiffStat {
                added: 7,
                deleted: 2,
                binary_files: 2,
            }
        );
        repository.git(&["config", "diff.relative", "true"]);
        let nested = discover(&repository.0.join("src"))
            .unwrap()
            .branch_changes
            .unwrap();
        assert_eq!(nested, changes);
    }

    #[test]
    fn pure_renames_have_zero_line_changes_and_changed_renames_use_destination_paths() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.write("src/café old\nname.rs", "pure rename\n");
        repository.write(
            "docs/old\nnotes.txt",
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n",
        );
        repository.write("src/old.bin", [0; 1024]);
        repository.commit_all();
        repository.git(&["switch", "-c", "feature"]);
        repository.git(&["mv", "src/café old\nname.rs", "src/café new\nname.rs"]);
        repository.commit_all();
        let pure = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(pure.total, shephrd::DiffStat::default());
        repository.git(&["mv", "docs/old\nnotes.txt", "src/new\nnotes.rs"]);
        repository.write(
            "src/new\nnotes.rs",
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\nchanged\n",
        );
        repository.git(&["mv", "src/old.bin", "docs/new.bin"]);
        repository.commit_all();
        let changed = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(
            changed.total,
            shephrd::DiffStat {
                added: 1,
                deleted: 1,
                binary_files: 1,
            }
        );
    }

    #[test]
    fn unavailable_bases_and_unborn_heads_are_not_zero_stats() {
        let repository = Repository::new();
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["init", "-b", "feature"]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["branch", "main"]);
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.total, shephrd::DiffStat::default());
        repository.git(&[
            "config",
            "--",
            "branch.feature.gh-merge-base",
            "--invalid-option",
        ]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["config", "branch.feature.gh-merge-base", ""]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["switch", "--orphan", "unrelated"]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["commit", "--allow-empty", "-m", "unrelated"]);
        repository.git(&["switch", "feature"]);
        repository.git(&["config", "branch.feature.gh-merge-base", "unrelated"]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
    }

    #[test]
    fn explicit_local_stacked_base_is_authoritative() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        repository.git(&["switch", "-c", "feature"]);
        repository.write("first.rs", "first\n");
        repository.commit_all();
        repository.git(&["branch", "stack/base"]);
        repository.write("second.rs", "second\nsecond\n");
        repository.commit_all();
        assert_eq!(
            discover(&repository.0)
                .unwrap()
                .branch_changes
                .unwrap()
                .total
                .added,
            3
        );
        repository.git(&["config", "branch.feature.gh-merge-base", "stack/base"]);
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.base_ref, "refs/heads/stack/base");
        assert_eq!(changes.total.added, 2);
        repository.git(&["config", "branch.feature.gh-merge-base", "missing/base"]);
        assert!(discover(&repository.0).unwrap().branch_changes.is_none());
        repository.git(&["checkout", "--detach"]);
        assert_eq!(
            discover(&repository.0)
                .unwrap()
                .branch_changes
                .unwrap()
                .total
                .added,
            3
        );
    }

    #[test]
    fn remote_heads_precede_main_and_same_name_upstream_is_not_a_base() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        let initial = resolve_commit(&repository.0, "HEAD").unwrap();
        repository.git(&["switch", "-c", "feature"]);
        repository.write("feature.rs", "feature\n");
        repository.commit_all();
        repository.git(&["config", "branch.feature.remote", "origin"]);
        repository.git(&["config", "branch.feature.merge", "refs/heads/feature"]);
        repository.git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        repository.git(&["update-ref", "refs/remotes/origin/main", &initial]);
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.base_ref, "refs/remotes/origin/main");
        assert_eq!(changes.total.added, 1);
        repository.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        repository.git(&["update-ref", "refs/remotes/origin/release", &initial]);
        repository.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/release",
        ]);
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.total.added, 1);
        repository.git(&["update-ref", "refs/remotes/upstream/trunk", "HEAD"]);
        repository.git(&[
            "symbolic-ref",
            "refs/remotes/upstream/HEAD",
            "refs/remotes/upstream/trunk",
        ]);
        repository.git(&["config", "branch.feature.remote", "upstream"]);
        let changes = discover(&repository.0).unwrap().branch_changes.unwrap();
        assert_eq!(changes.total, shephrd::DiffStat::default());
    }

    #[test]
    fn master_and_other_locally_available_remotes_are_fallbacks() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "master"]);
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        let initial = resolve_commit(&repository.0, "HEAD").unwrap();
        repository.git(&["switch", "-c", "feature"]);
        repository.write("feature.rs", "feature\n");
        repository.commit_all();
        assert_eq!(
            discover(&repository.0)
                .unwrap()
                .branch_changes
                .unwrap()
                .base_ref,
            "refs/heads/master"
        );
        repository.git(&["update-ref", "refs/remotes/another/master", &initial]);
        assert_eq!(
            discover(&repository.0)
                .unwrap()
                .branch_changes
                .unwrap()
                .base_ref,
            "refs/remotes/another/master"
        );
    }

    #[test]
    fn linked_worktree_branch_stats_use_its_head_and_common_local_base() {
        let repository = Repository::new();
        repository.git(&["init", "-b", "main"]);
        repository.git(&["commit", "--allow-empty", "-m", "initial"]);
        repository.git(&["branch", "stack/base"]);
        let worktree = repository.0.join("linked");
        repository.git(&[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            worktree.to_str().unwrap(),
        ]);
        repository.git(&["config", "branch.linked-branch.gh-merge-base", "stack/base"]);
        std::fs::write(worktree.join("linked.rs"), "linked\n").unwrap();
        repository.git_at(&worktree, &["add", "--all"]);
        repository.git_at(&worktree, &["commit", "-m", "linked"]);
        let root = discover(&repository.0).unwrap().branch_changes.unwrap();
        let (linked, paths) = discover_watched(&worktree).unwrap();
        let changes = linked.branch_changes.unwrap();
        assert_eq!(root.total, shephrd::DiffStat::default());
        assert_eq!(changes.total.added, 1);
        assert_eq!(changes.base_ref, "refs/heads/stack/base");
        assert!(paths.contains(&repository.0.join(".git/refs/heads/stack")));
    }
}
