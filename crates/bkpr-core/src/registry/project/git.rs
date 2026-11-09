//! Local Git ingestion adapted from main 9ad93bd, project.rs (base selection and
//! NUL numstat parsing) and project_watch.rs (bounded child termination). No helper,
//! Zellij delivery, forge calls, external diff or textconv participates here.
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{ChangeSummary, RepositoryInfo};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStringExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_DIRECTORIES: usize = 32_768;
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);
const CHILD_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub root: PathBuf,
    pub common: PathBuf,
    pub git_dir: PathBuf,
}
pub(super) struct WatchPlan {
    pub directories: BTreeSet<PathBuf>,
    pub external_files: BTreeSet<PathBuf>,
}

pub(super) struct Git<'a> {
    cwd: &'a Path,
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl<'a> Git<'a> {
    pub fn new(cwd: &'a Path, cancel: &'a AtomicBool) -> Self {
        Self {
            cwd,
            deadline: Instant::now() + INGEST_TIMEOUT,
            cancel,
        }
    }
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire),
            "project ingestion cancelled"
        );
        ensure!(
            Instant::now() < self.deadline,
            "project ingestion exceeded ten-second deadline"
        );
        Ok(())
    }
    fn output(&self, args: &[&str]) -> Result<Output> {
        self.check()?;
        let mut command = command(self.cwd);
        command.args(args);
        run(
            &mut command,
            self.deadline.min(Instant::now() + CHILD_TIMEOUT),
            self.cancel,
        )
    }
    fn required(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.output(args)?;
        ensure!(
            output.status.success(),
            "git {}: {}",
            args[0],
            diagnostic(&output.stderr)
        );
        Ok(output.stdout)
    }
    fn optional(&self, args: &[&str], absent: &[i32]) -> Result<Option<String>> {
        let output = self.output(args)?;
        if absent
            .iter()
            .any(|code| output.status.code() == Some(*code))
        {
            return Ok(None);
        }
        ensure!(
            output.status.success(),
            "git {}: {}",
            args[0],
            diagnostic(&output.stderr)
        );
        Ok(Some(
            String::from_utf8(output.stdout)
                .context("Git returned non-UTF-8 text metadata")?
                .trim_end_matches('\n')
                .to_owned(),
        ))
    }
    fn resolve(&self, reference: &str) -> Result<Option<String>> {
        self.optional(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
            &[1],
        )
    }
    fn config(&self, name: &str) -> Result<Option<String>> {
        self.optional(&["config", "--get", name], &[1])
    }
    pub fn identify(&self) -> Result<Option<Identity>> {
        ensure!(
            self.cwd.is_absolute() && self.cwd.is_dir(),
            "project CWD is not an existing absolute directory"
        );
        let output = self.output(&["rev-parse", "--show-toplevel"])?;
        if !output.status.success() {
            // Other failures (unsafe ownership, corrupt metadata, permissions) must not
            // masquerade as an ordinary directory outside a repository.
            if output.status.code() == Some(128)
                && String::from_utf8_lossy(&output.stderr).contains("not a git repository")
            {
                // A linked worktree whose .git target is unavailable reports the
                // same status/text as a normal nonrepository. An existing marker
                // (including a broken symlink) makes this an unknown/error, not
                // proof that its live canonical reservation can be released.
                for ancestor in self.cwd.ancestors() {
                    let marker = ancestor.join(".git");
                    match std::fs::symlink_metadata(&marker) {
                        Ok(_) => bail!("git discovery: {}", diagnostic(&output.stderr)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error)
                                .with_context(|| format!("inspecting {}", marker.display()));
                        }
                    }
                }
                return Ok(None);
            }
            bail!("git discovery: {}", diagnostic(&output.stderr));
        }
        let root = canonical_output(output.stdout)?;
        let common = canonical_output(self.required(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?)?;
        let git_dir = canonical_output(self.required(&["rev-parse", "--absolute-git-dir"])?)?;
        Ok(Some(Identity {
            root,
            common,
            git_dir,
        }))
    }
    pub fn repository(
        &self,
        identity: &Identity,
        previous: Option<&RepositoryInfo>,
    ) -> Result<RepositoryInfo> {
        Git {
            cwd: &identity.root,
            deadline: self.deadline,
            cancel: self.cancel,
        }
        .repository_at_root(identity, previous)
    }
    fn repository_at_root(
        &self,
        identity: &Identity,
        previous: Option<&RepositoryInfo>,
    ) -> Result<RepositoryInfo> {
        let (branch, head_sha) = if let Some(previous) = previous {
            (previous.branch.clone(), previous.head_sha.clone())
        } else {
            let symbolic = self.optional(&["symbolic-ref", "--quiet", "HEAD"], &[1])?;
            let branch = symbolic
                .as_deref()
                .and_then(|reference| reference.strip_prefix("refs/heads/"))
                .map(str::to_owned);
            (branch, self.resolve("HEAD")?)
        };
        self.reject_executable_filters()?;
        let staged = stats(&self.required(&[
            "diff",
            "--cached",
            "--numstat",
            "-z",
            "--find-renames=50%",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ])?)?;
        let unstaged = stats(&self.required(&[
            "diff",
            "--numstat",
            "-z",
            "--find-renames=50%",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ])?)?;
        let untracked = self.required(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        let untracked_files = paths(&untracked)?.count() as u64;
        // Worktree/index changes cannot change the selected base or committed
        // delta. Reuse the last observation until refs/config/attributes change.
        let (base_ref, base_sha, committed) = if let Some(previous) = previous {
            (
                previous.base_ref.clone(),
                previous.base_sha.clone(),
                previous.committed,
            )
        } else {
            self.base(branch.as_deref(), head_sha.as_deref())?
        };
        // Naming follows main: linked worktrees share the common repository name.
        let named_root = if identity
            .common
            .file_name()
            .is_some_and(|name| name == ".git")
        {
            identity.common.parent().unwrap_or(&identity.root)
        } else {
            &identity.root
        };
        let name = named_root
            .file_name()
            .unwrap_or(named_root.as_os_str())
            .to_string_lossy()
            .into_owned();
        Ok(RepositoryInfo {
            name,
            root: identity.root.clone(),
            common_dir: identity.common.clone(),
            git_dir: identity.git_dir.clone(),
            branch,
            head_sha,
            staged,
            unstaged,
            untracked_files,
            base_ref,
            base_sha,
            committed,
        })
    }
    fn reject_executable_filters(&self) -> Result<()> {
        let configured = self.output(&[
            "config",
            "--null",
            "--get-regexp",
            "^filter\\..*\\.(clean|process)$",
        ])?;
        if configured.status.code() == Some(1) {
            return Ok(());
        }
        ensure!(
            configured.status.success(),
            "reading Git filter configuration: {}",
            diagnostic(&configured.stderr)
        );
        let mut drivers = BTreeSet::new();
        for record in paths(&configured.stdout)? {
            let (key, value) = record.split_at(
                record
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .context("invalid Git filter configuration")?,
            );
            if value.len() <= 1 {
                continue;
            }
            let driver = key
                .strip_prefix(b"filter.")
                .context("invalid Git filter name")?;
            let end = driver
                .iter()
                .rposition(|byte| *byte == b'.')
                .context("invalid Git filter driver")?;
            drivers.insert(driver[..end].to_vec());
        }
        if drivers.is_empty() {
            return Ok(());
        }
        // Determining modified paths can itself run a clean/process filter.
        // Conservatively reject selected tracked drivers even in a clean tree;
        // do not disable them and pretend raw byte totals have Git semantics.
        let tracked = self.required(&["ls-files", "--cached", "--deduplicate", "-z"])?;
        if tracked.is_empty() {
            return Ok(());
        }
        self.check()?;
        let mut command = command(self.cwd);
        command.args(["check-attr", "-z", "--stdin", "filter"]);
        let output = run_input(
            &mut command,
            Some(&tracked),
            self.deadline.min(Instant::now() + CHILD_TIMEOUT),
            self.cancel,
        )?;
        ensure!(
            output.status.success(),
            "checking Git filter attributes: {}",
            diagnostic(&output.stderr)
        );
        let attributes = output
            .stdout
            .strip_suffix(&[0])
            .context("unterminated Git attributes")?;
        let mut attributes = attributes.split(|byte| *byte == 0);
        while let Some(_path) = attributes.next() {
            let attribute = attributes.next().context("missing Git attribute name")?;
            let value = attributes.next().context("missing Git attribute value")?;
            ensure!(attribute == b"filter", "unexpected Git attribute");
            if !matches!(value, b"set" | b"unset" | b"unspecified") && drivers.contains(value) {
                bail!(
                    "Git stats require a configured executable clean/process filter; not run (filtered tracked files are unsupported, including clean worktrees)"
                );
            }
        }
        Ok(())
    }
    /// Verified provider targets override local fallback selection, including an
    /// explicit gh-merge-base. Never resolve a branch name supplied by a provider
    /// or fetch its missing commit; unavailable targets remain unknown.
    pub(super) fn with_ci_base(
        &self,
        repository: &RepositoryInfo,
        base: Option<&bkpr_protocol::ChangeRequestBase>,
    ) -> Result<(Option<String>, Option<String>, Option<ChangeSummary>)> {
        let head = repository.head_sha.as_deref();
        ensure!(
            self.resolve("HEAD")?.as_deref() == head,
            "CI diff source HEAD changed"
        );
        let branch = self.optional(&["symbolic-ref", "--quiet", "HEAD"], &[1])?;
        ensure!(
            branch
                .as_deref()
                .and_then(|reference| reference.strip_prefix("refs/heads/"))
                == repository.branch.as_deref(),
            "CI diff source branch changed"
        );
        let result = if let Some(base) = base {
            ensure!(
                matches!(base.commit.len(), 40 | 64)
                    && base.commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid provider base commit"
            );
            if let Some(target) = self.resolve(&base.commit)?
                && let Some(head) = head
                && let Some(merge_base) = self.optional(&["merge-base", &target, head], &[1])?
            {
                let committed = stats(&self.required(&[
                    "diff",
                    "--numstat",
                    "-z",
                    "--find-renames=50%",
                    "--no-relative",
                    "--no-ext-diff",
                    "--no-textconv",
                    &merge_base,
                    head,
                    "--",
                ])?)?;
                (Some(base.branch.clone()), Some(merge_base), Some(committed))
            } else {
                (Some(base.branch.clone()), None, None)
            }
        } else {
            self.base(repository.branch.as_deref(), head)?
        };
        ensure!(
            self.resolve("HEAD")?.as_deref() == head,
            "CI diff source HEAD changed"
        );
        let branch = self.optional(&["symbolic-ref", "--quiet", "HEAD"], &[1])?;
        ensure!(
            branch
                .as_deref()
                .and_then(|reference| reference.strip_prefix("refs/heads/"))
                == repository.branch.as_deref(),
            "CI diff source branch changed"
        );
        Ok(result)
    }
    fn base(
        &self,
        branch: Option<&str>,
        head: Option<&str>,
    ) -> Result<(Option<String>, Option<String>, Option<ChangeSummary>)> {
        let configured = branch
            .map(|branch| self.config(&format!("branch.{branch}.gh-merge-base")))
            .transpose()?
            .flatten();
        let selected = if let Some(configured) = configured {
            // A missing explicit stacked-branch target is unknown, never a fallback.
            if configured.is_empty() {
                return Ok((Some(configured), None, None));
            }
            let commit = self.resolve(&configured)?;
            Some((configured, commit))
        } else {
            let refs = String::from_utf8(self.required(&["for-each-ref", "--format=%(refname)"])?)?;
            let names: BTreeSet<_> = refs.lines().collect();
            let remote = branch
                .map(|branch| self.config(&format!("branch.{branch}.remote")))
                .transpose()?
                .flatten()
                .filter(|remote| remote != ".");
            let mut remotes = Vec::new();
            if let Some(remote) = remote {
                remotes.push(remote);
            }
            if !remotes.iter().any(|name| name == "origin") {
                remotes.push("origin".to_owned());
            }
            let preferred = remotes.len();
            for reference in &names {
                if let Some(remote) = reference
                    .strip_prefix("refs/remotes/")
                    .and_then(|name| name.split_once('/').map(|(remote, _)| remote))
                    && !remotes.iter().any(|name| name == remote)
                {
                    remotes.push(remote.to_owned());
                }
            }
            remotes[preferred..].sort_unstable();
            let mut candidates: Vec<_> = remotes[..preferred]
                .iter()
                .map(|remote| format!("refs/remotes/{remote}/HEAD"))
                .collect();
            for remote in &remotes {
                for name in ["main", "master"] {
                    candidates.push(format!("refs/remotes/{remote}/{name}"));
                }
            }
            candidates.extend(["refs/heads/main".to_owned(), "refs/heads/master".to_owned()]);
            let mut selected = None;
            for reference in candidates {
                if names.contains(reference.as_str())
                    && let Some(commit) = self.resolve(&reference)?
                {
                    selected = Some((reference, Some(commit)));
                    break;
                }
            }
            selected
        };
        let Some((reference, target)) = selected else {
            return Ok((None, None, None));
        };
        let symbolic = self.optional(
            &[
                "rev-parse",
                "--symbolic-full-name",
                "--verify",
                "--quiet",
                "--end-of-options",
                &reference,
            ],
            &[1],
        )?;
        let reference = symbolic
            .filter(|name| !name.is_empty())
            .unwrap_or(reference);
        let (Some(target), Some(head)) = (target, head) else {
            return Ok((Some(reference), None, None));
        };
        let Some(merge_base) = self.optional(&["merge-base", &target, head], &[1])? else {
            return Ok((Some(reference), None, None));
        };
        let committed = stats(&self.required(&[
            "diff",
            "--numstat",
            "-z",
            "--find-renames=50%",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            &merge_base,
            head,
            "--",
        ])?)?;
        Ok((Some(reference), Some(merge_base), Some(committed)))
    }
    pub fn watches(&self, identity: Option<&Identity>) -> Result<WatchPlan> {
        let mut directories = BTreeSet::new();
        let mut external_files = BTreeSet::new();
        // Ancestor topology watches detect git init, removal and a nested repository.
        directories.extend(self.cwd.ancestors().map(Path::to_path_buf));
        // Partial git init must remain observable after .git itself was created.
        for ancestor in self.cwd.ancestors() {
            for path in [
                ancestor.join(".git"),
                ancestor.join(".git/refs"),
                ancestor.join(".git/refs/heads"),
            ] {
                if path.is_dir() {
                    directories.insert(path);
                }
            }
        }
        let Some(identity) = identity else {
            return Ok(WatchPlan {
                directories,
                external_files,
            });
        };
        // Maintained ignore rules keep build caches out of the native watch set.
        // Tracked parents are added separately: gitignore never hides tracked files.
        let mut configured = self.output(&["config", "--path", "--get", "core.excludesFile"])?;
        let excludes = if configured.status.success() {
            ensure!(
                configured.stdout.pop() == Some(b'\n'),
                "unterminated Git exclude path"
            );
            PathBuf::from(OsString::from_vec(configured.stdout))
        } else {
            ensure!(
                configured.status.code() == Some(1),
                "reading Git excludes: {}",
                diagnostic(&configured.stderr)
            );
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
                .unwrap_or_default()
                .join("git/ignore")
        };
        let excludes = if excludes.is_absolute() {
            excludes
        } else {
            self.cwd.join(excludes)
        };
        let mut builder = ignore::WalkBuilder::new(&identity.root);
        builder
            .hidden(false)
            .follow_links(false)
            .require_git(false)
            .ignore(false)
            .parents(false)
            .git_global(false)
            .git_exclude(false)
            .current_dir(identity.root.clone())
            .filter_entry(|entry| entry.file_name() != ".git");
        for ignore_file in [&excludes, &identity.common.join("info/exclude")] {
            if ignore_file.is_file()
                && let Some(error) = builder.add_ignore(ignore_file)
            {
                return Err(error).context("reading Git exclude rules");
            }
        }
        let walk = builder.build();
        for entry in walk {
            self.check()?;
            let entry = entry.context("walking project watch directories")?;
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                directories.insert(entry.into_path());
                ensure!(
                    directories.len() <= MAX_DIRECTORIES,
                    "project directory watch budget exceeded ({MAX_DIRECTORIES})"
                );
            }
        }
        let root_git = Git {
            cwd: &identity.root,
            deadline: self.deadline,
            cancel: self.cancel,
        };
        let tracked = root_git.required(&["ls-files", "--cached", "-z"])?;
        for path in paths(&tracked)? {
            let path = identity.root.join(OsString::from_vec(path.to_vec()));
            if let Some(parent) = path.parent() {
                for ancestor in parent
                    .ancestors()
                    .take_while(|path| path.starts_with(&identity.root))
                {
                    if ancestor.is_dir() {
                        directories.insert(ancestor.to_path_buf());
                    }
                }
            }
            ensure!(
                directories.len() <= MAX_DIRECTORIES,
                "project directory watch budget exceeded ({MAX_DIRECTORIES})"
            );
        }
        for path in [&identity.git_dir, &identity.common] {
            directories.insert(path.clone());
            if let Some(parent) = path.parent() {
                directories.insert(parent.to_path_buf());
            }
        }
        let mut pending = vec![identity.common.join("refs")];
        if identity.git_dir != identity.common {
            pending.push(identity.git_dir.join("refs"));
        }
        while let Some(path) = pending.pop() {
            self.check()?;
            if identity.git_dir != identity.common
                && ["refs/bisect", "refs/worktree", "refs/rewritten"]
                    .iter()
                    .any(|namespace| {
                        path.strip_prefix(&identity.common)
                            .is_ok_and(|relative| relative.starts_with(namespace))
                    })
            {
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            directories.insert(path.clone());
            ensure!(
                directories.len() <= MAX_DIRECTORIES,
                "project directory watch budget exceeded ({MAX_DIRECTORIES})"
            );
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                self.check()?;
                ensure!(
                    pending.len() + directories.len() <= MAX_DIRECTORIES,
                    "project metadata watch budget exceeded ({MAX_DIRECTORIES})"
                );
                if entry.file_type()?.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        external_files.insert(excludes);
        // Configuration includes and global ignore files can live outside this
        // repository. Exact file matching avoids waking Git for unrelated home,
        // /etc or XDG directory writes; parent watches survive atomic replacement.
        let origins =
            self.required(&["config", "--show-origin", "--name-only", "--null", "--list"])?;
        let mut origins = paths(&origins)?;
        while let Some(origin) = origins.next() {
            origins.next().context("missing Git configuration key")?;
            if let Some(path) = origin.strip_prefix(b"file:") {
                let path = PathBuf::from(OsString::from_vec(path.to_vec()));
                external_files.insert(if path.is_absolute() {
                    path
                } else {
                    self.cwd.join(path)
                });
                ensure!(
                    directories.len() + external_files.len() <= MAX_DIRECTORIES,
                    "project watch plan budget exceeded ({MAX_DIRECTORIES})"
                );
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            external_files.insert(PathBuf::from(home).join(".gitconfig"));
        }
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        {
            external_files.insert(config.join("git/config"));
        }
        for file in &external_files {
            self.check()?;
            if let Some(parent) = file.ancestors().skip(1).find(|path| path.is_dir()) {
                directories.insert(parent.to_path_buf());
            }
        }
        let info = identity.common.join("info");
        if info.is_dir() {
            directories.insert(info);
        }
        ensure!(
            directories.len() + external_files.len() <= MAX_DIRECTORIES,
            "project watch plan budget exceeded ({MAX_DIRECTORIES})"
        );
        Ok(WatchPlan {
            directories,
            external_files,
        })
    }
}

fn canonical_output(mut output: Vec<u8>) -> Result<PathBuf> {
    ensure!(
        output.pop() == Some(b'\n'),
        "Git returned an unterminated path"
    );
    let path = PathBuf::from(OsString::from_vec(output));
    ensure!(
        path.is_absolute(),
        "Git returned a relative repository identity"
    );
    std::fs::canonicalize(path).context("canonicalizing repository identity")
}
fn paths(bytes: &[u8]) -> Result<impl Iterator<Item = &[u8]>> {
    ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "Git returned unterminated paths"
    );
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty()))
}
fn stats(bytes: &[u8]) -> Result<ChangeSummary> {
    let mut total = ChangeSummary::default();
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            ensure!(
                records.next().is_none(),
                "Git returned an empty diff record"
            );
            break;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().context("missing insertion count")?;
        let deleted = fields.next().context("missing deletion count")?;
        let path = fields.next().context("missing diff path")?;
        if path.is_empty() {
            ensure!(
                records.next().is_some_and(|path| !path.is_empty())
                    && records.next().is_some_and(|path| !path.is_empty()),
                "incomplete rename paths"
            );
        }
        total.files = total
            .files
            .checked_add(1)
            .context("diff file count overflow")?;
        if added == b"-" && deleted == b"-" {
            total.binary_files = total
                .binary_files
                .checked_add(1)
                .context("binary count overflow")?;
        } else {
            total.insertions = total
                .insertions
                .checked_add(std::str::from_utf8(added)?.parse()?)
                .context("insertion count overflow")?;
            total.deletions = total
                .deletions
                .checked_add(std::str::from_utf8(deleted)?.parse()?)
                .context("deletion count overflow")?;
        }
    }
    Ok(total)
}

fn diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(2048)])
        .trim()
        .to_owned()
}
pub(crate) fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    // Remove all launch-level overrides, including GIT_CONFIG_COUNT/PARAMETERS,
    // index/object/shallow namespaces and tracing. User/repository config remains.
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-C",
        ])
        .arg(cwd);
    command
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "setting Git pipe nonblocking: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
pub(crate) fn run(command: &mut Command, deadline: Instant, cancel: &AtomicBool) -> Result<Output> {
    run_input(command, None, deadline, cancel)
}
fn run_input(
    command: &mut Command,
    input: Option<&[u8]>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Output> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        let parent = libc::getpid();
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .context("starting local Git metadata command")?;
    let result = (|| {
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut stdin = child.stdin.take();
        let mut remaining = input.unwrap_or_default();
        if let Some(stdin) = &stdin {
            nonblocking(stdin.as_raw_fd())?;
        }
        nonblocking(stdout.as_raw_fd())?;
        nonblocking(stderr.as_raw_fd())?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut eof = [false; 2];
        let mut scratch = [0u8; 8192];
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire),
                "project ingestion cancelled"
            );
            ensure!(Instant::now() < deadline, "Git metadata command timed out");
            if remaining.is_empty() {
                stdin.take();
            } else if let Some(writer) = &mut stdin {
                match writer.write(remaining) {
                    Ok(0) => bail!("Git metadata input closed"),
                    Ok(count) => remaining = &remaining[count..],
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            for (index, (reader, bytes)) in [
                (&mut stdout as &mut dyn Read, &mut out),
                (&mut stderr as &mut dyn Read, &mut err),
            ]
            .into_iter()
            .enumerate()
            {
                loop {
                    match reader.read(&mut scratch) {
                        Ok(0) => {
                            eof[index] = true;
                            break;
                        }
                        Ok(count) => {
                            ensure!(
                                bytes.len() + count <= MAX_OUTPUT,
                                "Git metadata output exceeds {MAX_OUTPUT} bytes"
                            );
                            bytes.extend_from_slice(&scratch[..count]);
                            ensure!(
                                Instant::now() < deadline && !cancel.load(Ordering::Acquire),
                                "Git metadata command cancelled or timed out"
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if let Some(status) = child.try_wait()?
                && eof.iter().all(|eof| *eof)
            {
                ensure!(
                    remaining.is_empty(),
                    "Git exited before reading complete metadata input"
                );
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            let mut fds = [
                libc::pollfd {
                    fd: if eof[0] { -1 } else { stdout.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if eof[1] { -1 } else { stderr.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stdin.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1),
                    events: libc::POLLOUT,
                    revents: 0,
                },
            ];
            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(50) as i32;
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
        }
    })();
    if result.is_err() {
        // Kill the group even if the leader already exited: inherited pipes must
        // not let a descendant survive cancellation or hold the worker forever.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests;
