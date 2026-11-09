use super::*;
use bkpr_protocol::{CiFetchState, CiStatus, PaneInfo, ProjectSnapshot};
use serde_json::json;

struct Fixture {
    registry: Registry,
    worker: Worker,
    root: tempfile::TempDir,
    _state: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(state.path()).unwrap();
        registry.stop_projects();
        registry.stop_ci();
        let (_, wake) = crossbeam_channel::bounded(1);
        let (auth_sender, auth_events) = crossbeam_channel::bounded(256);
        let worker = Worker {
            registry: Arc::downgrade(&registry.inner),
            cancel: Arc::new(AtomicBool::new(false)),
            wake,
            monitor: Monitor::new(),
            bindings: BTreeMap::new(),
            next_source_generation: 0,
            published_version: 0,
            auth_dirty: Arc::new(AtomicBool::new(false)),
            auth_watcher: None,
            auth_watched: BTreeSet::new(),
            auth_retry: None,
            notify_wake: registry.inner.ci_wake.clone(),
            auth_config: Some(state.path().join("gh")),
            auth_directories: BTreeSet::new(),
            auth_events,
            auth_sender,
            auth_overflow: Arc::new(AtomicBool::new(false)),
        };
        let fixture = Self {
            registry,
            worker,
            root: tempfile::tempdir().unwrap(),
            _state: state,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["commit", "--allow-empty", "-m", "fixture"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/fixture/project",
        ]);
        fixture
    }
    fn git(&self, args: &[&str]) {
        let output = project::git::command(self.root.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
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
    fn pane(&self, id: &str, cwd: &Path) {
        let cancel = AtomicBool::new(false);
        let repository = project::inspect_repository(cwd, &cancel).unwrap();
        self.registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .registry
            .panes
            .push(PaneInfo {
                id: id.into(),
                workspace_id: None,
                window_id: None,
                title: id.into(),
                status: PaneStatus::Running,
                exit_code: None,
                cwd: cwd.to_owned(),
                home: self.root.path().to_owned(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                isolated_environment: true,
                project: Some(ProjectSnapshot {
                    cwd: cwd.to_owned(),
                    state: ProjectState::Ready,
                    color_slot: Some(0),
                    repository: Some(repository),
                    error: None,
                    ci: None,
                }),
            });
    }
    fn sync(&mut self) {
        assert!(self.worker.sync_inputs(&self.registry.inner));
        self.worker.publish(&self.registry.inner);
    }
    fn project(&self, id: &str) -> ProjectSnapshot {
        self.registry
            .snapshot()
            .unwrap()
            .panes
            .into_iter()
            .find(|pane| pane.id == id)
            .unwrap()
            .project
            .unwrap()
    }
    fn pass(&mut self, id: &str, remote_sha: &str) {
        let source = self.worker.bindings[id].source.clone().unwrap();
        let commit = json!({"oid":remote_sha,"statusCheckRollup":{"contexts":{"nodes":[{
            "__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS",
            "startedAt":"2026-10-13T12:00:00Z","completedAt":"2026-10-13T12:01:42Z"
        }],"pageInfo":{"hasNextPage":false}}}});
        self.worker.monitor.finish_json_fixture(std::slice::from_ref(&source), json!({"data":{
            "r0":{"object":{"target":commit},"b0":{"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}},"c0":{"nodes":[]}},
            "rateLimit":{"remaining":5000}
        }}));
        self.worker.publish(&self.registry.inner);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.registry.shutdown().unwrap();
    }
}

#[test]
fn provider_remote_tip_and_local_head_remain_distinct_and_ordinary_wakes_are_idle() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    fixture.pass("pane", "provider-remote-tip");
    let ci = fixture.project("pane").ci.unwrap();
    assert_eq!(ci.status, CiStatus::Passed);
    assert_eq!(ci.fetch_state, CiFetchState::Ready);
    assert_eq!(ci.head_sha, "provider-remote-tip");
    assert_ne!(ci.head_sha, ci.local_head_sha);
    assert_eq!(ci.checks[0].duration_seconds, Some(102));
    assert_eq!(fixture.worker.monitor.timeout(), -1);
    let revision = fixture.registry.snapshot().unwrap().revision;
    let source = fixture.worker.bindings["pane"].source.clone();
    // With no targeted metadata event even a changed file cannot cause source
    // rediscovery. Native watcher tests separately exercise event routing.
    fixture.git(&[
        "remote",
        "set-url",
        "origin",
        "https://github.com/other/project",
    ]);
    fixture
        .registry
        .inner
        .ci_input_generation
        .fetch_add(1, Ordering::AcqRel);
    fixture.sync();
    assert_eq!(fixture.worker.bindings["pane"].source, source);
    assert_eq!(fixture.project("pane").ci, Some(ci));
    assert_eq!(fixture.registry.snapshot().unwrap().revision, revision);
    assert_eq!(fixture.worker.monitor.timeout(), -1);
    fixture
        .registry
        .inner
        .ci_inputs
        .lock()
        .unwrap()
        .insert(fixture.root.path().to_owned(), 1);
    fixture.sync();
    assert_eq!(
        fixture.worker.bindings["pane"]
            .source
            .as_ref()
            .unwrap()
            .repository
            .path,
        "other/project"
    );
    assert_eq!(
        fixture.project("pane").ci.unwrap().fetch_state,
        CiFetchState::Loading
    );
}

#[test]
fn tracking_only_push_changes_revive_completed_queries_without_changing_project_fields() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    fixture.pass("pane", "old-provider-tip");
    let before = fixture.project("pane").repository;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture
        .registry
        .inner
        .ci_inputs
        .lock()
        .unwrap()
        .insert(fixture.root.path().to_owned(), 1);
    fixture.sync();
    assert_eq!(fixture.project("pane").repository, before);
    assert!(
        fixture.worker.bindings["pane"]
            .source
            .as_ref()
            .unwrap()
            .remote_branch
            .as_ref()
            .unwrap()
            .tracking_sha
            .is_some()
    );
    assert!(fixture.worker.monitor.timeout() >= 0);
    assert_eq!(
        fixture.project("pane").ci.unwrap().fetch_state,
        CiFetchState::Loading
    );
}

#[test]
fn manual_refresh_is_exact_and_does_not_cross_item_cwd_auth_contexts() {
    let mut fixture = Fixture::new();
    let nested = fixture.root.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fixture.pane("root", fixture.root.path());
    fixture.pane("nested", &nested);
    fixture.sync();
    fixture.sync();
    fixture.pass("root", "same-remote-tip");
    let project = fixture.project("root");
    let ci = project.ci.unwrap();
    let target = CiRefreshTarget {
        pane_id: "root".into(),
        cwd: project.cwd,
        repository: ci.repository.clone(),
        head_sha: ci.head_sha.clone(),
        local_head_sha: ci.local_head_sha.clone(),
        checked_ref: ci.checked_ref.clone(),
    };
    fixture.registry.refresh_ci(&target).unwrap();
    let queued = fixture.registry.inner.ci_refresh.lock().unwrap().clone();
    assert_eq!(queued, vec![target.clone()]);
    let wrong = CiRefreshTarget {
        cwd: nested,
        ..target
    };
    assert!(fixture.registry.refresh_ci(&wrong).is_err());
    let root = fixture.worker.bindings["root"].source.as_ref().unwrap();
    let nested = fixture.worker.bindings["nested"].source.as_ref().unwrap();
    assert_ne!(root.cwd, nested.cwd);
    assert_eq!(
        fixture.worker.monitor.snapshot_ref(root).unwrap().status,
        CiStatus::Passed
    );
    assert_eq!(
        fixture
            .worker
            .monitor
            .snapshot_ref(nested)
            .unwrap()
            .fetch_state,
        CiFetchState::Loading
    );
}

#[test]
fn identical_active_cwd_subscribers_share_discovery_generation_and_completed_cache() {
    let mut fixture = Fixture::new();
    fixture.pane("first", fixture.root.path());
    fixture.pane("second", fixture.root.path());
    fixture.sync();
    let source = fixture.worker.bindings["first"].source.clone().unwrap();
    assert_eq!(
        fixture.worker.bindings["second"].source.as_ref(),
        Some(&source)
    );
    assert_eq!(fixture.worker.next_source_generation, 1);
    fixture.pass("first", "shared-provider-tip");
    assert_eq!(fixture.project("first").ci, fixture.project("second").ci);
    let generation = fixture.worker.next_source_generation;
    fixture.git(&[
        "remote",
        "set-url",
        "origin",
        "https://github.com/not-rediscovered/project",
    ]);
    fixture.pane("joining", fixture.root.path());
    fixture.sync();
    assert_eq!(fixture.worker.next_source_generation, generation);
    assert_eq!(
        fixture.worker.bindings["joining"].source.as_ref(),
        Some(&source)
    );
    assert_eq!(fixture.project("joining").ci, fixture.project("first").ci);
    assert_eq!(fixture.worker.monitor.timeout(), -1);
}

#[test]
fn metadata_a_to_b_to_a_cannot_publish_an_unpublished_old_response() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let old = fixture.worker.bindings["pane"].source.clone().unwrap();
    let value = json!({"data":{"r0":{
        "object":{"target":{"oid":"retired-provider-tip","statusCheckRollup":null}},
        "b0":{"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}},
        "c0":{"nodes":[]}
    },"rateLimit":{"remaining":5000}}});
    fixture
        .worker
        .monitor
        .finish_json_fixture(std::slice::from_ref(&old), value.clone());
    fixture
        .registry
        .inner
        .ci_inputs
        .lock()
        .unwrap()
        .insert(fixture.root.path().to_owned(), 2);
    fixture.sync();
    let current = fixture.worker.bindings["pane"].source.as_ref().unwrap();
    assert_ne!(current.generation, old.generation);
    let ci = fixture.project("pane").ci.unwrap();
    assert_eq!(ci.fetch_state, CiFetchState::Loading);
    assert!(ci.head_sha.is_empty());
    fixture
        .worker
        .monitor
        .finish_json_fixture(std::slice::from_ref(&old), value);
    fixture.worker.publish(&fixture.registry.inner);
    assert_eq!(
        fixture.project("pane").ci.unwrap().fetch_state,
        CiFetchState::Loading
    );
    assert!(fixture.project("pane").ci.unwrap().head_sha.is_empty());
}

#[test]
fn auth_retirement_drops_old_account_facts_and_forces_a_cold_query() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    fixture.pass("pane", "old-account-tip");
    let source = fixture.worker.bindings["pane"].source.clone().unwrap();
    fixture.worker.monitor.finish_json_fixture(
        std::slice::from_ref(&source),
        json!({
            "data":null,"errors":[{"type":"RATE_LIMITED","path":["r0"]}]
        }),
    );
    fixture.worker.retire_auth(&fixture.registry.inner);
    assert!(fixture.project("pane").ci.is_none());
    fixture.sync();
    let new_source = fixture.worker.bindings["pane"].source.as_ref().unwrap();
    assert_ne!(new_source.generation, source.generation);
    let cold = fixture.project("pane").ci.unwrap();
    assert_eq!(cold.fetch_state, CiFetchState::Loading);
    assert!(cold.checks.is_empty());
    assert!(cold.head_sha.is_empty());
    assert!((0..=2000).contains(&fixture.worker.monitor.timeout()));
    fixture.pass("pane", "new-account-tip");
    assert_eq!(
        fixture.project("pane").ci.unwrap().head_sha,
        "new-account-tip"
    );
}

#[test]
fn verified_pr_base_projects_local_committed_diff_and_missing_base_stays_unknown() {
    let mut fixture = Fixture::new();
    let output = project::git::command(fixture.root.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let base = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    fs::write(fixture.root.path().join("child"), "committed child\n").unwrap();
    fixture.git(&["add", "child"]);
    fixture.git(&["commit", "-m", "child"]);
    fixture.git(&["config", "branch.main.gh-merge-base", "missing-local-base"]);
    fs::write(
        fixture.root.path().join("child"),
        "worktree changes\nmust not count\n",
    )
    .unwrap();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let source = fixture.worker.bindings["pane"].source.clone().unwrap();
    let mut value = json!({"data":{"r0":{
        "object":{"target":{"oid":"actual-published-tip","statusCheckRollup":null}},
        "b0":{"associatedPullRequests":{"nodes":[{
            "number":7,"title":"Child","url":"https://github.com/fixture/project/pull/7",
            "state":"OPEN","isDraft":false,"updatedAt":"2026-10-13T12:00:00Z",
            "headRefOid":"actual-published-tip","headRefName":"main",
            "headRepository":{"nameWithOwner":"fixture/project"},
            "baseRefName":"stack/provider-parent","baseRefOid":base,
            "repository":{"nameWithOwner":"fixture/project","url":"https://github.com/fixture/project"},
            "body":"Full searchable request body","comments":{"nodes":[]}
        }],"pageInfo":{"hasNextPage":false}}},"c0":{"nodes":[]}
    },"rateLimit":{"remaining":5000}}});
    fixture
        .worker
        .monitor
        .finish_json_fixture(std::slice::from_ref(&source), value.clone());
    fixture.worker.publish(&fixture.registry.inner);
    let project = fixture.project("pane");
    let repository = project.repository.unwrap();
    let ci = project.ci.unwrap();
    assert_eq!(ci.head_sha, "actual-published-tip");
    assert_eq!(repository.head_sha.as_ref(), Some(&ci.local_head_sha));
    assert_eq!(
        repository.base_ref.as_deref(),
        Some("stack/provider-parent")
    );
    assert_eq!(repository.committed.unwrap().insertions, 1);
    value["data"]["r0"]["b0"]["associatedPullRequests"]["nodes"][0]["baseRefOid"] =
        json!("f".repeat(40));
    fixture
        .worker
        .monitor
        .finish_json_fixture(std::slice::from_ref(&source), value);
    fixture.worker.publish(&fixture.registry.inner);
    let repository = fixture.project("pane").repository.unwrap();
    assert_eq!(
        repository.base_ref.as_deref(),
        Some("stack/provider-parent")
    );
    assert_eq!(repository.base_sha, None);
    assert_eq!(repository.committed, None);
}

#[test]
fn cold_auth_failure_can_be_manually_refreshed_by_ref_before_a_checked_sha_exists() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let source = fixture.worker.bindings["pane"].source.clone().unwrap();
    fixture.worker.monitor.finish_json_fixture(
        std::slice::from_ref(&source),
        json!({
            "data":null,"errors":[{"type":"UNAUTHORIZED"}]
        }),
    );
    fixture.worker.publish(&fixture.registry.inner);
    let project = fixture.project("pane");
    let ci = project.ci.unwrap();
    assert_eq!(ci.fetch_state, CiFetchState::Unavailable);
    assert!(ci.head_sha.is_empty());
    assert_eq!(ci.checked_ref.as_deref(), Some("origin/main"));
    let target = CiRefreshTarget {
        pane_id: "pane".into(),
        cwd: project.cwd,
        repository: ci.repository,
        head_sha: String::new(),
        local_head_sha: ci.local_head_sha,
        checked_ref: ci.checked_ref,
    };
    fixture.registry.refresh_ci(&target).unwrap();
    let mut queued = fixture
        .registry
        .inner
        .ci_refresh
        .lock()
        .unwrap()
        .pop()
        .unwrap();
    queued.cwd = source.cwd.clone();
    fixture.worker.monitor.refresh(&queued);
    fixture.worker.publish(&fixture.registry.inner);
    let retrying = fixture.project("pane").ci.unwrap();
    assert_eq!(retrying.fetch_state, CiFetchState::Loading);
    assert!(retrying.error.is_some());
    assert!((0..=2000).contains(&fixture.worker.monitor.timeout()));
    fixture.pass("pane", "known-after-auth-unlock");
    let ready = fixture.project("pane").ci.unwrap();
    assert_eq!(ready.fetch_state, CiFetchState::Ready);
    assert_eq!(ready.head_sha, "known-after-auth-unlock");
    assert!(ready.error.is_none());
}

#[test]
fn cwd_relative_auth_config_is_watched_without_retiring_on_unrelated_parent_events() {
    let mut fixture = Fixture::new();
    fixture.worker.auth_config = Some(PathBuf::from(".gh-private"));
    fs::create_dir(fixture.root.path().join(".gh-private")).unwrap();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let cwd = fixture.worker.bindings["pane"]
        .source
        .as_ref()
        .unwrap()
        .cwd
        .clone();
    let config = cwd.join(".gh-private");
    assert!(fixture.worker.auth_directories.contains(&config));
    fixture
        .worker
        .auth_sender
        .send(Ok(notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Content),
        ))
        .add_path(cwd.join("ordinary-file"))))
        .unwrap();
    assert!(!fixture.worker.auth_events_changed());
    fixture
        .worker
        .auth_sender
        .send(Ok(notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Content),
        ))
        .add_path(config.join("hosts.yml"))))
        .unwrap();
    assert!(fixture.worker.auth_events_changed());
}

#[test]
fn publication_ignores_unrelated_global_churn_but_rejects_a_changed_target_epoch() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let source = fixture.worker.bindings["pane"].source.clone().unwrap();
    let response = |head: &str| {
        json!({"data":{"r0":{
        "object":{"target":{"oid":head,"statusCheckRollup":null}},
        "b0":{"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}},
        "c0":{"nodes":[]}
    },"rateLimit":{"remaining":5000}}})
    };
    fixture.worker.monitor.finish_json_fixture(
        std::slice::from_ref(&source),
        response("ready-despite-status-churn"),
    );
    fixture
        .registry
        .inner
        .ci_input_generation
        .fetch_add(100, Ordering::AcqRel);
    fixture.worker.publish(&fixture.registry.inner);
    let published = fixture.project("pane").ci.unwrap();
    assert_eq!(published.fetch_state, CiFetchState::Ready);
    assert_eq!(published.head_sha, "ready-despite-status-churn");
    let revision = fixture.registry.snapshot().unwrap().revision;
    fixture.worker.monitor.finish_json_fixture(
        std::slice::from_ref(&source),
        response("retired-target-result"),
    );
    let epoch = fixture
        .registry
        .inner
        .ci_input_generation
        .fetch_add(1, Ordering::AcqRel)
        + 1;
    fixture
        .registry
        .inner
        .ci_inputs
        .lock()
        .unwrap()
        .insert(fixture.root.path().to_owned(), epoch);
    fixture.worker.publish(&fixture.registry.inner);
    assert_eq!(fixture.project("pane").ci, Some(published));
    assert_eq!(fixture.registry.snapshot().unwrap().revision, revision);
}

#[test]
fn publication_rejects_new_working_stats_errors_without_a_metadata_epoch_change() {
    let mut fixture = Fixture::new();
    fixture.pane("pane", fixture.root.path());
    fixture.sync();
    let previous = fixture.project("pane").ci;
    let source = fixture.worker.bindings["pane"].source.clone().unwrap();
    fixture.worker.monitor.finish_json_fixture(
        std::slice::from_ref(&source),
        json!({"data":{"r0":{
        "object":{"target":{"oid":"must-not-publish","statusCheckRollup":null}},
        "b0":{"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}},
        "c0":{"nodes":[]}
    },"rateLimit":{"remaining":5000}}}),
    );
    {
        let mut state = fixture.registry.inner.state.lock().unwrap();
        let project = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|pane| pane.id == "pane")
            .unwrap()
            .project
            .as_mut()
            .unwrap();
        project.error = Some("working stats unavailable".into());
        project.state = ProjectState::Error;
    }
    fixture.worker.publish(&fixture.registry.inner);
    assert_eq!(fixture.project("pane").ci, previous);
}
