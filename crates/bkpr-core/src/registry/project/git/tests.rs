use super::*;
use std::{fs, os::unix::ffi::OsStrExt};

struct Repository(tempfile::TempDir);
impl Repository {
    fn new() -> Self {
        let repository = Self(tempfile::tempdir().unwrap());
        repository.git(&["init", "-b", "main"]);
        repository
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn git(&self, args: &[&str]) {
        let output = command(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "-c",
                "user.name=Bkpr Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        let path = self.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn commit(&self) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "-m", "fixture"]);
    }
    fn inspect(&self) -> RepositoryInfo {
        let cancel = AtomicBool::new(false);
        let git = Git::new(self.path(), &cancel);
        let identity = git.identify().unwrap().unwrap();
        git.repository(&identity).unwrap()
    }
}

#[test]
fn staged_unstaged_deleted_renamed_and_binary_files_keep_separate_totals() {
    let repository = Repository::new();
    repository.write("renamed\nü.txt", "one\ntwo\nthree\n");
    repository.write("deleted", "delete me\n");
    repository.write("binary", b"\0old");
    repository.commit();
    repository.git(&["mv", "renamed\nü.txt", "moved\tü.txt"]);
    repository.write("binary", b"\0new");
    repository.git(&["add", "binary"]);
    repository.write("moved\tü.txt", "one\ntwo\nthree\nfour\n");
    fs::remove_file(repository.path().join("deleted")).unwrap();
    repository.write("untracked/one", "untracked\n");
    let snapshot = repository.inspect();
    assert_eq!(snapshot.staged.files, 2);
    assert_eq!(snapshot.staged.binary_files, 1);
    assert_eq!(snapshot.staged.insertions + snapshot.staged.deletions, 0);
    assert_eq!(snapshot.unstaged.files, 2);
    assert_eq!(snapshot.unstaged.insertions, 1);
    assert_eq!(snapshot.unstaged.deletions, 1);
    assert_eq!(snapshot.untracked_files, 1);
}

#[test]
fn committed_summary_is_whole_tree_merge_base_not_sum_of_commits() {
    let repository = Repository::new();
    repository.write("tracked", "base\n");
    repository.commit();
    repository.git(&["switch", "-c", "feature"]);
    repository.write("tracked", "base\ntemporary\n");
    repository.commit();
    repository.write("tracked", "base\n");
    repository.commit();
    let snapshot = repository.inspect();
    assert_eq!(snapshot.base_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(snapshot.committed.unwrap().files, 0);
    repository.git(&[
        "config",
        "branch.feature.gh-merge-base",
        "missing-stack-parent",
    ]);
    let snapshot = repository.inspect();
    assert_eq!(snapshot.base_ref.as_deref(), Some("missing-stack-parent"));
    assert!(snapshot.base_sha.is_none() && snapshot.committed.is_none());
}

#[test]
fn remote_head_precedes_main_but_feature_upstream_is_not_its_own_base() {
    let repository = Repository::new();
    repository.write("tracked", "base\n");
    repository.commit();
    repository.git(&["branch", "trunk"]);
    repository.git(&["switch", "-c", "feature"]);
    repository.write("new", "feature\n");
    repository.commit();
    repository.git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
    repository.git(&[
        "update-ref",
        "refs/remotes/origin/trunk",
        "refs/heads/trunk",
    ]);
    repository.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    repository.git(&["config", "branch.feature.remote", "origin"]);
    repository.git(&["config", "branch.feature.merge", "refs/heads/feature"]);
    let snapshot = repository.inspect();
    assert_eq!(
        snapshot.base_ref.as_deref(),
        Some("refs/remotes/origin/trunk")
    );
    assert_eq!(snapshot.committed.unwrap().insertions, 1);
}

#[test]
fn unborn_detached_and_linked_worktrees_have_independent_heads_and_shared_common_identity() {
    let repository = Repository::new();
    repository.write("tracked", "initial\n");
    repository.git(&["add", "tracked"]);
    let unborn = repository.inspect();
    assert_eq!(unborn.branch.as_deref(), Some("main"));
    assert!(unborn.head_sha.is_none() && unborn.committed.is_none());
    assert_eq!(unborn.staged.insertions, 1);
    repository.commit();
    let linked = tempfile::tempdir().unwrap();
    let path = linked.path().join("linked");
    repository.git(&["worktree", "add", "-b", "linked", path.to_str().unwrap()]);
    repository.git(&["switch", "--detach"]);
    let detached = repository.inspect();
    let cancel = AtomicBool::new(false);
    let git = Git::new(&path, &cancel);
    let identity = git.identify().unwrap().unwrap();
    let linked = git.repository(&identity).unwrap();
    assert!(detached.branch.is_none());
    assert_eq!(linked.branch.as_deref(), Some("linked"));
    assert_eq!(detached.common_dir, linked.common_dir);
    assert_ne!(detached.root, linked.root);
    assert_ne!(detached.git_dir, linked.git_dir);
}

#[test]
fn subdirectory_discovery_counts_the_entire_worktree_and_preserves_non_utf8_cwds() {
    let repository = Repository::new();
    repository.write("outside", "outside\n");
    repository.commit();
    repository.write("untracked", "outside\n");
    let subdirectory = repository
        .path()
        .join(OsString::from_vec(b"sub\xff\n".to_vec()));
    fs::create_dir(&subdirectory).unwrap();
    let cancel = AtomicBool::new(false);
    let git = Git::new(&subdirectory, &cancel);
    let identity = git.identify().unwrap().unwrap();
    let snapshot = git.repository(&identity).unwrap();
    assert_eq!(snapshot.untracked_files, 1);
    assert_eq!(
        identity.root.as_os_str().as_bytes(),
        repository.path().as_os_str().as_bytes()
    );
}

#[test]
fn watch_coverage_honors_git_not_tool_ignore_and_keeps_ignored_tracked_parents() {
    let repository = Repository::new();
    repository.write("tracked/one", "tracked\n");
    repository.commit();
    repository.write(".gitignore", "tracked/\nbuild/\n");
    repository.write(".ignore", "visible/\n");
    repository.write("build/objects/file", "ignored\n");
    repository.write("visible/empty/marker", "visible\n");
    let cancel = AtomicBool::new(false);
    let git = Git::new(repository.path(), &cancel);
    let identity = git.identify().unwrap().unwrap();
    let watched = git.watches(Some(&identity)).unwrap().directories;
    assert!(watched.contains(&repository.path().join("tracked")));
    assert!(watched.contains(&repository.path().join("visible/empty")));
    assert!(!watched.contains(&repository.path().join("build/objects")));
}

#[test]
fn bounded_child_deadline_kills_and_reaps_the_actual_process() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("pid");
    let cancel = AtomicBool::new(false);
    let mut child = Command::new("/bin/sh");
    child
        .args(["-c", "echo $$ > \"$1\"; exec sleep 60", "fixture"])
        .arg(&pid_file);
    assert!(run(&mut child, Instant::now() + Duration::from_secs(1), &cancel).is_err());
    let pid: i32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn explicit_stacked_parent_takes_precedence_over_default_main() {
    let repository = Repository::new();
    repository.write("base", "base\n");
    repository.commit();
    repository.git(&["switch", "-c", "parent"]);
    repository.write("parent", "parent\n");
    repository.commit();
    repository.git(&["switch", "-c", "child"]);
    repository.write("child", "child\n");
    repository.commit();
    repository.git(&["config", "branch.child.gh-merge-base", "parent"]);
    let snapshot = repository.inspect();
    assert_eq!(snapshot.base_ref.as_deref(), Some("refs/heads/parent"));
    assert_eq!(snapshot.committed.unwrap().files, 1);
}

#[test]
fn repository_paths_with_non_utf8_and_newlines_remain_exact() {
    let repository = Repository::new();
    let root = repository
        .path()
        .join(OsString::from_vec(b"repo\xff\n".to_vec()));
    fs::create_dir(&root).unwrap();
    let output = command(&root)
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let cancel = AtomicBool::new(false);
    let git = Git::new(&root, &cancel);
    let identity = git.identify().unwrap().unwrap();
    assert_eq!(identity.root, root);
    let snapshot = git.repository(&identity).unwrap();
    assert_eq!(snapshot.git_dir, root.join(".git"));
}

#[test]
fn configured_executable_filters_are_rejected_only_when_selected_by_tracked_paths() {
    let repository = Repository::new();
    repository.write("tracked", "initial\n");
    repository.commit();
    let marker = repository.path().join("filter-was-executed");
    let executable = format!("printf executed > '{}'; cat", marker.display());
    repository.git(&["config", "filter.forbidden.clean", &executable]);
    assert!(repository.inspect().head_sha.is_some());
    repository.write(".gitattributes", "tracked filter=forbidden\n");
    repository.write("tracked", "changed\n");
    let cancel = AtomicBool::new(false);
    let git = Git::new(repository.path(), &cancel);
    let identity = git.identify().unwrap().unwrap();
    let error = git.repository(&identity).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("executable clean/process filter")
    );
    assert!(!marker.exists());
}
