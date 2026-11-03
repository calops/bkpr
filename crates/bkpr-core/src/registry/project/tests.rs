use super::*;

struct Fixture {
    registry: Registry,
    _state: tempfile::TempDir,
    repo: tempfile::TempDir,
    _home: tempfile::TempDir,
    pane: String,
}
impl Fixture {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let registry = Registry::open(state.path()).unwrap();
        let value = registry
            .request(Request::new(Operation::PaneSpawn {
                workspace_id: None,
                window_id: None,
                expected_revision: None,
                spec: PaneSpawnSpec {
                    argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                    cwd: repo.path().to_owned(),
                    home: home.path().to_owned(),
                    size: Size {
                        cols: 80,
                        rows: 24,
                        cell_width: 8,
                        cell_height: 16,
                    },
                    title: Some("project fixture".into()),
                    isolated_environment: true,
                },
            }))
            .unwrap();
        Self {
            registry,
            _state: state,
            repo,
            _home: home,
            pane: value["pane_id"].as_str().unwrap().to_owned(),
        }
    }
    fn git(&self, args: &[&str]) {
        let output = self.git_output(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn git_output(&self, args: &[&str]) -> std::process::Output {
        let mut command = std::process::Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"GIT_") {
                command.env_remove(name);
            }
        }
        command
            .args(["-C"])
            .arg(self.repo.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap()
    }
    #[track_caller]
    fn wait(&self, ready: impl Fn(&ProjectSnapshot) -> bool) -> ProjectSnapshot {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut state = self.registry.inner.state.lock().unwrap();
        loop {
            let snapshot = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == self.pane)
                .unwrap()
                .project
                .clone();
            if let Some(snapshot) = &snapshot
                && ready(snapshot)
            {
                return snapshot.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(state);
                let paths = self.git_output(&["ls-files", "--others", "--exclude-standard", "-z"]);
                panic!(
                    "project did not converge: {snapshot:?}; untracked paths: {:?}; Git stderr: {:?}",
                    String::from_utf8_lossy(&paths.stdout),
                    String::from_utf8_lossy(&paths.stderr),
                );
            }
            state = self
                .registry
                .inner
                .changed
                .wait_timeout(state, remaining)
                .unwrap()
                .0;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.registry.shutdown().unwrap();
    }
}

#[test]
fn native_watch_discovers_git_init_and_an_untracked_file_in_a_new_nested_directory() {
    let fixture = Fixture::new();
    fixture.wait(|project| project.state == ProjectState::NotRepository);
    fixture.git(&["init", "-b", "main"]);
    let initialized = fixture.wait(|project| project.state == ProjectState::Ready);
    assert!(initialized.repository.unwrap().head_sha.is_none());
    fs::create_dir_all(fixture.repo.path().join("new/empty/nested")).unwrap();
    fs::write(fixture.repo.path().join("new/empty/nested/file"), "new\n").unwrap();
    let project = fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.untracked_files == 1)
    });
    assert_eq!(project.cwd, fixture.repo.path());
    fixture.git(&["add", "new/empty/nested/file"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project.repository.as_ref().is_some_and(|repository| {
                repository.untracked_files == 0 && repository.staged.insertions == 1
            })
    });
    fs::write(
        fixture.repo.path().join("new/empty/nested/file"),
        "new\nchanged\n",
    )
    .unwrap();
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.unstaged.insertions == 1)
    });
}

#[test]
fn live_project_metadata_is_neither_durable_nor_restored_as_a_live_fact() {
    let fixture = Fixture::new();
    fixture.wait(|project| project.state == ProjectState::NotRepository);
    fixture
        .registry
        .request(Request::new(Operation::PaneRename {
            pane_id: fixture.pane.clone(),
            title: "durability boundary".into(),
        }))
        .unwrap();
    let bytes = fs::read(fixture._state.path().join("registry.json")).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(record["registry"]["panes"][0].get("project").is_none());

    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut record = record;
    record["registry"]["panes"][0]["project"] = serde_json::to_value(ProjectSnapshot {
        cwd: fixture.repo.path().to_owned(),
        state: ProjectState::Ready,
        color_slot: Some(37),
        repository: None,
        error: Some("not a live fact".into()),
    })
    .unwrap();
    fs::write(
        root.path().join("registry.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        root.path().join("registry.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let reopened = Registry::open(root.path()).unwrap();
    let pane = &reopened.snapshot().unwrap().panes[0];
    assert!(pane.project.is_none());
    assert_eq!(pane.status, PaneStatus::Stopped);
    reopened.shutdown().unwrap();
}

#[test]
fn configured_external_ignore_changes_rebuild_live_untracked_watch_coverage() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-b", "main"]);
    fs::create_dir(fixture.repo.path().join("ignored")).unwrap();
    fs::write(fixture.repo.path().join("ignored/one"), "one\n").unwrap();
    let excludes = fixture._state.path().join("custom-excludes");
    fs::write(&excludes, "ignored/\n").unwrap();
    fixture.git(&["config", "core.excludesFile", excludes.to_str().unwrap()]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.untracked_files == 0)
    });
    // The configured file is outside the worktree, with an arbitrary name.
    fs::write(&excludes, "").unwrap();
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.untracked_files == 1)
    });
    fs::write(fixture.repo.path().join("ignored/two"), "two\n").unwrap();
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.untracked_files == 2)
    });
}

#[test]
fn dropping_a_public_registry_clone_preserves_the_live_project_service() {
    let fixture = Fixture::new();
    fixture.wait(|project| project.state == ProjectState::NotRepository);
    drop(fixture.registry.clone());
    fixture.git(&["init", "-b", "main"]);
    fixture.wait(|project| project.state == ProjectState::Ready);
}

/// Drive the real worker's discovery/refresh/publication boundaries without
/// scheduler sleeps or native-watch timing. Native coverage has its own tests.
struct Publication {
    registry: Registry,
    worker: Worker,
    root: tempfile::TempDir,
    _state: tempfile::TempDir,
}
impl Publication {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(state.path()).unwrap();
        registry.stop_projects();
        let (_, wake) = crossbeam_channel::bounded(1);
        let (event_sender, events) = crossbeam_channel::bounded(256);
        let worker = Worker {
            registry: Arc::downgrade(&registry.inner),
            cancel: Arc::new(AtomicBool::new(false)),
            wake,
            events,
            overflow: Arc::new(AtomicBool::new(false)),
            watcher: None,
            event_sender,
            notify_wake: registry.inner.project_wake.clone(),
            installed: BTreeSet::new(),
            bindings: BTreeMap::new(),
            cache: BTreeMap::new(),
            owners: BTreeMap::new(),
            color_slots: BTreeMap::new(),
            rebind: false,
            inputs_pending: false,
            watch_retry: None,
            retry_delay: Duration::from_secs(2),
        };
        Self {
            registry,
            worker,
            root: tempfile::tempdir().unwrap(),
            _state: state,
        }
    }
    fn git(&self, cwd: &Path, args: &[&str]) {
        let mut command = std::process::Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .arg("-C")
            .arg(cwd)
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
    fn repository(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "-b", "main"]);
        path
    }
    fn linked(&self, repository: &Path) -> PathBuf {
        self.git(repository, &["commit", "--allow-empty", "-m", "fixture"]);
        let path = self.root.path().join("linked");
        self.git(
            repository,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        path
    }
    fn pane(&self, id: &str, cwd: &Path) {
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
                project: Some(loading(cwd, None)),
                isolated_environment: true,
            });
    }
    fn cwd(&self, id: &str, cwd: &Path) {
        let mut state = self.registry.inner.state.lock().unwrap();
        let pane = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|pane| pane.id == id)
            .unwrap();
        let color_slot = pane.project.as_ref().and_then(|project| project.color_slot);
        pane.project = Some(loading(cwd, color_slot));
        pane.foreground_cwd = Some(cwd.to_owned());
    }
    fn exit(&self, id: &str) {
        let mut state = self.registry.inner.state.lock().unwrap();
        let pane = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|pane| pane.id == id)
            .unwrap();
        pane.status = PaneStatus::Exited;
        pane.project = None;
        state.project_retired.insert(id.to_owned());
    }
    fn ingest(&mut self) {
        loop {
            assert!(self.worker.sync_inputs());
            if !self.worker.inputs_pending {
                break;
            }
        }
        let keys: Vec<_> = self.worker.cache.keys().cloned().collect();
        for key in keys {
            // Exercise production identity/metadata/publication, excluding only
            // watch installation and its asynchronous coverage invalidation.
            self.worker.cache.get_mut(&key).unwrap().rebuild = false;
            self.worker.refresh(&key);
        }
        self.worker.publish();
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
}
impl Drop for Publication {
    fn drop(&mut self) {
        self.registry.shutdown().unwrap();
    }
}

#[test]
fn project_ordinals_follow_sorted_new_pane_ids_and_reuse_only_released_slots() {
    let mut fixture = Publication::new();
    let first = fixture.repository("z-first");
    let second = fixture.repository("m-second");
    let third = fixture.repository("a-third");
    fixture.pane("pane-30", &third);
    fixture.pane("pane-20", &second);
    fixture.pane("pane-10", &first);
    fixture.ingest();
    assert_eq!(fixture.project("pane-10").color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    assert_eq!(fixture.project("pane-30").color_slot, Some(2));

    fixture.exit("pane-10");
    fixture.worker.publish();
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    assert_eq!(fixture.project("pane-30").color_slot, Some(2));
    let fourth = fixture.repository("new-low-slot");
    fixture.pane("pane-40", &fourth);
    fixture.ingest();
    assert_eq!(fixture.project("pane-40").color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    assert_eq!(fixture.project("pane-30").color_slot, Some(2));
}

#[test]
fn linked_worktrees_share_the_canonical_slot_until_the_last_owner_exits() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let linked = fixture.linked(&repository);
    let other = fixture.repository("other");
    fixture.pane("pane-10", &repository);
    fixture.pane("pane-20", &linked);
    fixture.pane("pane-30", &other);
    fixture.ingest();
    let root_project = fixture.project("pane-10");
    let linked_project = fixture.project("pane-20");
    assert_eq!(root_project.color_slot, Some(0));
    assert_eq!(linked_project.color_slot, Some(0));
    assert_eq!(
        root_project.repository.unwrap().common_dir,
        linked_project.repository.unwrap().common_dir
    );
    assert_eq!(fixture.project("pane-30").color_slot, Some(1));

    fixture.exit("pane-10");
    fixture.ingest();
    assert_eq!(fixture.project("pane-20").color_slot, Some(0));
    let newcomer = fixture.repository("newcomer");
    fixture.pane("pane-40", &newcomer);
    fixture.ingest();
    assert_eq!(fixture.project("pane-40").color_slot, Some(2));
    fixture.exit("pane-20");
    fixture.ingest();
    let replacement = fixture.repository("replacement");
    fixture.pane("pane-50", &replacement);
    fixture.ingest();
    assert_eq!(fixture.project("pane-50").color_slot, Some(0));
    assert_eq!(fixture.project("pane-30").color_slot, Some(1));
    assert_eq!(fixture.project("pane-40").color_slot, Some(2));
}

#[test]
fn unknown_cwd_retains_its_reservation_until_verified_repository_reassociation() {
    let mut fixture = Publication::new();
    let first = fixture.repository("first");
    let second = fixture.repository("second");
    fixture.pane("pane-10", &first);
    fixture.pane("pane-20", &second);
    fixture.ingest();
    let unavailable = fixture.root.path().join("temporarily-unavailable");
    fixture.cwd("pane-10", &unavailable);
    fixture.worker.publish();
    let loading = fixture.project("pane-10");
    assert_eq!(loading.state, ProjectState::Loading);
    assert_eq!(loading.color_slot, Some(0));
    assert!(loading.repository.is_none());
    fixture.ingest();
    let failed = fixture.project("pane-10");
    assert_eq!(failed.state, ProjectState::Error);
    assert_eq!(failed.color_slot, Some(0));
    assert!(failed.error.is_some());
    assert!(failed.repository.is_none());

    let third = fixture.repository("third");
    fixture.pane("pane-30", &third);
    fixture.ingest();
    assert_eq!(fixture.project("pane-30").color_slot, Some(2));
    fixture.cwd("pane-10", &second);
    fixture.ingest();
    let reassociated = fixture.project("pane-10");
    assert_eq!(reassociated.state, ProjectState::Ready);
    assert_eq!(reassociated.color_slot, Some(1));
    assert_eq!(
        reassociated.repository.unwrap().common_dir,
        fixture.project("pane-20").repository.unwrap().common_dir
    );
    let fourth = fixture.repository("fourth");
    fixture.pane("pane-40", &fourth);
    fixture.ingest();
    assert_eq!(fixture.project("pane-40").color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    assert_eq!(fixture.project("pane-30").color_slot, Some(2));
}

#[test]
fn broken_linked_metadata_keeps_error_truth_and_known_nonrepo_releases_the_slot() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let linked = fixture.linked(&repository);
    let other = fixture.repository("other");
    fixture.pane("pane-10", &linked);
    fixture.pane("pane-20", &other);
    fixture.ingest();
    let marker = linked.join(".git");
    let original = fs::read(&marker).unwrap();
    let unavailable = fixture.root.path().join("unavailable-gitdir");
    fs::write(&marker, format!("gitdir: {}\n", unavailable.display())).unwrap();
    let key = fixture.worker.bindings.get(&linked).unwrap().clone();
    fixture
        .worker
        .cache
        .get_mut(&key)
        .unwrap()
        .invalidate(false);
    fixture.worker.publish();
    let loading = fixture.project("pane-10");
    assert_eq!(loading.state, ProjectState::Loading);
    assert_eq!(loading.color_slot, Some(0));
    fixture.worker.rebind = true;
    fixture.ingest();
    let failed = fixture.project("pane-10");
    assert_eq!(failed.state, ProjectState::Error);
    assert!(failed.error.is_some());
    assert_eq!(failed.color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    fs::write(&marker, original).unwrap();
    fixture.worker.rebind = true;
    fixture.ingest();
    let repaired = fixture.project("pane-10");
    assert_eq!(repaired.state, ProjectState::Ready);
    assert!(repaired.error.is_none());
    assert_eq!(repaired.color_slot, Some(0));

    let plain = fixture.root.path().join("plain");
    fs::create_dir(&plain).unwrap();
    fixture.cwd("pane-10", &plain);
    fixture.ingest();
    let nonrepo = fixture.project("pane-10");
    assert_eq!(nonrepo.state, ProjectState::NotRepository);
    assert!(nonrepo.color_slot.is_none());
    assert!(nonrepo.repository.is_none());
    assert!(nonrepo.error.is_none());
    let newcomer = fixture.repository("newcomer");
    fixture.pane("pane-30", &newcomer);
    fixture.ingest();
    assert_eq!(fixture.project("pane-30").color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
}

#[test]
fn restoration_cannot_inherit_a_retired_executions_unresolved_reservation() {
    let mut fixture = Publication::new();
    let first = fixture.repository("first");
    let second = fixture.repository("second");
    fixture.pane("pane-10", &first);
    fixture.pane("pane-20", &second);
    fixture.ingest();
    assert_eq!(fixture.project("pane-10").color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));

    // Coalesce completion and restoration before the worker's next turn.
    fixture.exit("pane-10");
    let unavailable = fixture.root.path().join("restored-unavailable-cwd");
    {
        let mut state = fixture.registry.inner.state.lock().unwrap();
        let restored = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|pane| pane.id == "pane-10")
            .unwrap();
        restored.status = PaneStatus::Running;
        restored.foreground_cwd = Some(unavailable.clone());
        restored.project = Some(loading(&unavailable, None));
    }
    fixture.ingest();
    let restored = fixture.project("pane-10");
    assert_eq!(restored.state, ProjectState::Error);
    assert!(restored.color_slot.is_none());
    let third = fixture.repository("third");
    fixture.pane("pane-30", &third);
    fixture.ingest();
    assert_eq!(fixture.project("pane-30").color_slot, Some(0));
    fixture.cwd("pane-10", &first);
    fixture.ingest();
    assert_eq!(fixture.project("pane-10").color_slot, Some(2));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    assert_eq!(fixture.project("pane-30").color_slot, Some(0));
}

#[test]
fn native_completion_retires_project_metadata() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-b", "main"]);
    fixture.wait(|project| project.state == ProjectState::Ready && project.color_slot.is_some());
    let pane = fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&fixture.pane)
        .unwrap()
        .clone();
    pane.request(PaneRequest::Shutdown).unwrap();
    fixture.registry.record_completion(&fixture.pane).unwrap();
    let snapshot = fixture.registry.snapshot().unwrap();
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == fixture.pane)
        .unwrap();
    assert_eq!(pane.status, PaneStatus::Exited);
    assert!(pane.project.is_none());
}
