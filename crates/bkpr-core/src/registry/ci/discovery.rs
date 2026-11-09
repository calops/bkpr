//! Push-target discovery uses only bounded local Git commands; never git fetch.
use super::super::project::git;
use super::monitor::{CiSource, RemoteBranch};
use anyhow::{Context, Result, ensure};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub(super) fn discover(
    cwd: &Path,
    branch: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Option<CiSource>> {
    let reader = Reader {
        cwd,
        cancel,
        deadline: Instant::now() + Duration::from_secs(10),
    };
    let canonical_cwd = std::fs::canonicalize(cwd).context("canonicalizing CI source CWD")?;
    let Some(sha) = reader.text(&["rev-parse", "--verify", "--quiet", "HEAD"], &[1, 128])? else {
        return Ok(None);
    };
    ensure!(
        matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid local CI HEAD"
    );
    let branch = branch.filter(|branch| !branch.starts_with("detached @ "));
    let remote = reader.push_remote(branch)?;
    let Some(url) = reader.text(&["remote", "get-url", "--push", &remote], &[2])? else {
        return Ok(None);
    };
    let Some(repository) = super::monitor::github::repository(&url) else {
        return Ok(None);
    };
    let remote_branch = branch
        .map(|branch| -> Result<_> {
            let name = reader.remote_branch(&remote, branch)?;
            let tracking_sha = reader.text(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("refs/remotes/{remote}/{name}"),
                ],
                &[1, 128],
            )?;
            Ok(RemoteBranch {
                remote,
                name,
                tracking_sha,
            })
        })
        .transpose()?;
    Ok(Some(CiSource {
        cwd: canonical_cwd,
        generation: 0,
        repository,
        sha,
        branch: branch.map(str::to_owned),
        remote_branch,
    }))
}

struct Reader<'a> {
    cwd: &'a Path,
    cancel: &'a AtomicBool,
    deadline: Instant,
}
impl Reader<'_> {
    fn text(&self, args: &[&str], absent: &[i32]) -> Result<Option<String>> {
        let output = git::run(
            git::command(self.cwd).args(args),
            self.deadline.min(Instant::now() + Duration::from_secs(3)),
            self.cancel,
        )?;
        if absent
            .iter()
            .any(|code| output.status.code() == Some(*code))
        {
            return Ok(None);
        }
        // Do not leak remote URLs, credentials or wrapper stderr into live metadata.
        ensure!(
            output.status.success(),
            "local CI Git source discovery failed ({})",
            args[0]
        );
        Ok(Some(
            String::from_utf8(output.stdout)
                .context("non-UTF-8 CI source metadata")?
                .trim_end_matches('\n')
                .to_owned(),
        ))
    }
    fn config(&self, key: &str) -> Result<Option<String>> {
        self.text(&["config", "--get", key], &[1])
    }
    fn push_remote(&self, branch: Option<&str>) -> Result<String> {
        if let Some(branch) = branch
            && let Some(remote) = self.config(&format!("branch.{branch}.pushRemote"))?
        {
            return Ok(remote);
        }
        if let Some(remote) = self.config("remote.pushDefault")? {
            return Ok(remote);
        }
        if let Some(branch) = branch
            && let Some(remote) = self.config(&format!("branch.{branch}.remote"))?
        {
            return Ok(remote);
        }
        Ok("origin".to_owned())
    }
    fn remote_branch(&self, remote: &str, branch: &str) -> Result<String> {
        let local_ref = format!("refs/heads/{branch}");
        if let Some(specs) = self.text(
            &["config", "--get-all", &format!("remote.{remote}.push")],
            &[1],
        )? {
            for spec in specs.lines() {
                let spec = spec.strip_prefix('+').unwrap_or(spec);
                if matches!(spec, ":" | "HEAD") {
                    return Ok(branch.to_owned());
                }
                let (from, to) = spec.split_once(':').unwrap_or((spec, spec));
                if from == local_ref || from == branch || from == "HEAD" {
                    return Ok(to.strip_prefix("refs/heads/").unwrap_or(to).to_owned());
                }
                if let Some((prefix, suffix)) = from.split_once('*')
                    && let Some(middle) = local_ref
                        .strip_prefix(prefix)
                        .and_then(|value| value.strip_suffix(suffix))
                    && to.contains('*')
                {
                    let target = to.replacen('*', middle, 1);
                    return Ok(target
                        .strip_prefix("refs/heads/")
                        .unwrap_or(&target)
                        .to_owned());
                }
            }
        }
        if matches!(
            self.config("push.default")?.as_deref(),
            Some("current" | "matching")
        ) {
            return Ok(branch.to_owned());
        }
        if self.config(&format!("branch.{branch}.remote"))?.as_deref() == Some(remote)
            && let Some(merge) = self.config(&format!("branch.{branch}.merge"))?
            && let Some(name) = merge.strip_prefix("refs/heads/")
        {
            return Ok(name.to_owned());
        }
        Ok(branch.to_owned())
    }
}
