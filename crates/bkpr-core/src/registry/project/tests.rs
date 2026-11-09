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
                panes: vec![PaneSpawnEntry {
                    reference: "pane".into(),
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
                }],
                layout: None,
            }))
            .unwrap()
            .into_json()
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
        .unwrap()
        .into_json()
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
            canonical_cwds: BTreeMap::new(),
            cache: BTreeMap::new(),
            owners: BTreeMap::new(),
            color_slots: BTreeMap::new(),
            rebind: BTreeSet::new(),
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
    fn coverage(&mut self) {
        for entry in self.worker.cache.values_mut() {
            let git = git::Git::new(&entry.source, &self.worker.cancel);
            let plan = git.watches(entry.identity.as_ref()).unwrap();
            entry.directories = plan.directories;
            entry.external_files = plan.external_files;
            entry.rebuild = false;
        }
        self.worker.installed = self
            .worker
            .cache
            .values()
            .flat_map(|entry| entry.directories.iter().cloned())
            .collect();
    }
    fn event(&mut self, kind: EventKind, paths: &[PathBuf]) {
        self.worker
            .event_sender
            .send(Ok(Event {
                paths: paths.to_vec(),
                ..Event::new(kind)
            }))
            .unwrap();
        self.worker.drain_events();
    }
    fn refresh_dirty(&mut self) {
        while self.worker.inputs_pending || !self.worker.rebind.is_empty() {
            assert!(self.worker.sync_inputs());
        }
        let dirty: Vec<_> = self
            .worker
            .cache
            .iter()
            .filter(|(_, entry)| entry.due.is_some())
            .map(|(key, _)| key.clone())
            .collect();
        for key in dirty {
            // Native watch installation is covered by Fixture tests.
            self.worker.cache.get_mut(&key).unwrap().rebuild = false;
            self.worker.refresh(&key);
        }
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
    fixture.coverage();
    let marker = linked.join(".git");
    let original = fs::read(&marker).unwrap();
    let unavailable = fixture.root.path().join("unavailable-gitdir");
    let replacement = linked.join("replacement-git-marker");
    fs::write(&replacement, format!("gitdir: {}\n", unavailable.display())).unwrap();
    fs::rename(&replacement, &marker).unwrap();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[replacement.clone(), marker.clone()],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([linked.clone()]));
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.worker.publish();
    let loading = fixture.project("pane-10");
    assert_eq!(loading.state, ProjectState::Loading);
    assert_eq!(loading.color_slot, Some(0));
    fixture.refresh_dirty();
    let failed = fixture.project("pane-10");
    assert_eq!(failed.state, ProjectState::Error);
    assert!(failed.error.is_some());
    assert_eq!(failed.color_slot, Some(0));
    assert_eq!(fixture.project("pane-20").color_slot, Some(1));
    fs::write(&replacement, original).unwrap();
    fs::rename(&replacement, &marker).unwrap();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[replacement, marker],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([linked.clone()]));
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.worker.publish();
    let retry = fixture.project("pane-10");
    assert_eq!(retry.state, ProjectState::Error);
    assert_eq!(retry.error, failed.error);
    assert_eq!(retry.repository, failed.repository);
    fixture.refresh_dirty();
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
    pane.request(PaneRequest::Shutdown)
        .unwrap()
        .into_json()
        .unwrap();
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

#[test]
fn worktree_and_local_index_events_do_not_recheck_linked_or_unrelated_repositories() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let linked = fixture.linked(&repository);
    let other = fixture.repository("other");
    fixture.pane("main", &repository);
    fixture.pane("linked", &linked);
    fixture.pane("other", &other);
    fixture.ingest();
    fixture.coverage();
    let before = fixture.project("main");
    let linked_generation = fixture.worker.cache[&linked].generation;
    let other_generation = fixture.worker.cache[&other].generation;
    fs::write(repository.join("new"), "new\n").unwrap();
    fixture.event(
        EventKind::Create(notify::event::CreateKind::File),
        &[repository.join("new")],
    );
    fixture.worker.publish();
    let loading = fixture.project("main");
    assert_eq!(loading.state, ProjectState::Loading);
    assert_eq!(loading.repository, before.repository);
    assert_eq!(loading.color_slot, before.color_slot);
    assert!(fixture.worker.cache[&repository].identity_known);
    assert!(!fixture.worker.cache[&repository].metadata_dirty);
    assert!(fixture.worker.rebind.is_empty());
    assert!(fixture.worker.cache[&linked].due.is_none());
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.refresh_dirty();
    assert_eq!(
        fixture.project("main").repository.unwrap().untracked_files,
        1
    );
    assert_eq!(fixture.worker.cache[&linked].generation, linked_generation);
    assert_eq!(fixture.worker.cache[&other].generation, other_generation);

    fixture.git(&repository, &["add", "new"]);
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[
            repository.join(".git/index.lock"),
            repository.join(".git/index"),
        ],
    );
    assert!(fixture.worker.cache[&repository].rebuild);
    assert!(!fixture.worker.cache[&repository].metadata_dirty);
    assert!(fixture.worker.cache[&linked].due.is_none());
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.refresh_dirty();
    assert_eq!(
        fixture
            .project("main")
            .repository
            .unwrap()
            .staged
            .insertions,
        1
    );
}

#[test]
fn shared_refs_and_their_directory_renames_refresh_all_and_only_common_repository_worktrees() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let linked = fixture.linked(&repository);
    let other = fixture.repository("other");
    fixture.pane("main", &repository);
    fixture.pane("linked", &linked);
    fixture.pane("other", &other);
    fixture.ingest();
    fixture.coverage();
    fixture.git(&repository, &["branch", "stack/base", "HEAD"]);
    let refs = repository.join(".git/refs/heads/stack");
    fixture.event(
        EventKind::Create(notify::event::CreateKind::Folder),
        std::slice::from_ref(&refs),
    );
    for root in [&repository, &linked] {
        assert!(fixture.worker.cache[root].due.is_some());
        assert!(fixture.worker.cache[root].metadata_dirty);
        assert!(fixture.worker.cache[root].rebuild);
    }
    assert!(fixture.worker.cache[&other].due.is_none());
    assert!(fixture.worker.rebind.is_empty());
    fixture.refresh_dirty();
    fixture.coverage();
    let renamed = repository.join(".git/refs/heads/renamed");
    fs::rename(&refs, &renamed).unwrap();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[refs.clone(), renamed],
    );
    assert!(!fixture.worker.installed.contains(&refs));
    assert!(fixture.worker.cache[&repository].rebuild);
    assert!(fixture.worker.cache[&linked].rebuild);
    assert!(fixture.worker.cache[&other].due.is_none());
    assert!(fixture.worker.rebind.is_empty());
}

#[test]
fn nested_git_marker_rebinds_only_descendant_cwds_and_sibling_markers_do_nothing() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let nested = repository.join("nested");
    fs::create_dir(&nested).unwrap();
    let other = fixture.repository("other");
    fixture.pane("root", &repository);
    fixture.pane("nested", &nested);
    fixture.pane("other", &other);
    fixture.ingest();
    fixture.coverage();
    let sibling = fixture.repository("sibling");
    fixture.event(
        EventKind::Create(notify::event::CreateKind::Folder),
        &[sibling.join(".git")],
    );
    assert!(fixture.worker.rebind.is_empty());
    assert!(
        fixture
            .worker
            .cache
            .values()
            .all(|entry| entry.due.is_none())
    );
    fixture.git(&nested, &["init", "-b", "nested"]);
    fixture.event(
        EventKind::Create(notify::event::CreateKind::Folder),
        &[nested.join(".git")],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([nested.clone()]));
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.refresh_dirty();
    assert_eq!(fixture.project("root").repository.unwrap().root, repository);
    assert_eq!(fixture.project("nested").repository.unwrap().root, nested);
    assert!(fixture.worker.cache[&other].due.is_none());

    fs::remove_dir_all(nested.join(".git")).unwrap();
    fixture.coverage();
    fixture.event(
        EventKind::Remove(notify::event::RemoveKind::Folder),
        &[nested.join(".git")],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([nested.clone()]));
    fixture.refresh_dirty();
    assert_eq!(fixture.worker.bindings[&nested], repository);
    assert_eq!(
        fixture.project("nested").repository.unwrap().root,
        repository
    );
}

#[test]
fn pending_events_cannot_publish_a_completed_observation_as_fresh() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    fixture.pane("pane", &repository);
    fixture.ingest();
    fixture.coverage();
    let before = fixture.project("pane");
    fs::write(repository.join("new"), "new\n").unwrap();
    fixture
        .worker
        .cache
        .get_mut(&repository)
        .unwrap()
        .invalidate(false);
    // Queued after the coordinator's pre-refresh drain, just like an event
    // arriving while Git is running; refresh must drain it before publication.
    fixture
        .worker
        .event_sender
        .send(Ok(Event::new(EventKind::Create(
            notify::event::CreateKind::File,
        ))
        .add_path(repository.join("new"))))
        .unwrap();
    fixture.worker.refresh(&repository);
    let pending = fixture.project("pane");
    assert_eq!(pending.state, ProjectState::Loading);
    assert_eq!(pending.color_slot, before.color_slot);
    assert!(pending.repository.is_some());
    assert!(fixture.worker.cache[&repository].due.is_some());
    fixture.refresh_dirty();
    assert_eq!(fixture.project("pane").state, ProjectState::Ready);
}

#[test]
fn lost_events_invalidate_every_binding_and_retain_facts_until_full_resynchronization() {
    let mut fixture = Publication::new();
    let first = fixture.repository("first");
    let second = fixture.repository("second");
    fixture.pane("first", &first);
    fixture.pane("second", &second);
    fixture.ingest();
    fixture.coverage();
    fixture.worker.overflow.store(true, Ordering::Release);
    fixture.worker.drain_events();
    assert_eq!(
        fixture.worker.rebind,
        BTreeSet::from([first.clone(), second.clone()])
    );
    assert!(fixture.worker.installed.is_empty());
    for id in ["first", "second"] {
        let project = fixture.project(id);
        assert_eq!(project.state, ProjectState::Error);
        assert!(project.error.is_some());
        assert!(project.repository.is_some());
    }
    assert!(fixture.worker.cache.values().all(|entry| {
        !entry.identity_known && entry.metadata_dirty && entry.rebuild && entry.due.is_some()
    }));
}

#[test]
fn index_addition_installs_watches_for_preexisting_ignored_tracked_directories() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-b", "main"]);
    fs::write(fixture.repo.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir(fixture.repo.path().join("ignored")).unwrap();
    fs::write(fixture.repo.path().join("ignored/tracked"), "one\n").unwrap();
    fixture.git(&["add", ".gitignore"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project.repository.as_ref().is_some_and(|repository| {
                repository.untracked_files == 0 && repository.staged.insertions == 1
            })
    });
    fixture.git(&["add", "--force", "ignored/tracked"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.staged.insertions == 2)
    });
    fs::write(fixture.repo.path().join("ignored/tracked"), "one\ntwo\n").unwrap();
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project
                .repository
                .as_ref()
                .is_some_and(|repository| repository.unstaged.insertions == 1)
    });
}

#[test]
fn symlinked_cwd_rebinds_on_its_canonical_nested_marker() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let nested = repository.join("nested");
    fs::create_dir(&nested).unwrap();
    let alias = fixture.root.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();
    fixture.pane("alias", &alias);
    fixture.ingest();
    fixture.coverage();
    fixture.git(&nested, &["init", "-b", "nested"]);
    fixture.event(
        EventKind::Create(notify::event::CreateKind::Folder),
        &[nested.join(".git")],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([alias.clone()]));
    fixture.refresh_dirty();
    let project = fixture.project("alias");
    assert_eq!(project.cwd, alias);
    assert_eq!(project.repository.unwrap().root, nested);
}

#[test]
fn recreated_directory_paths_retire_watches_even_when_the_name_already_exists_again() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let watched = repository.join("watched");
    fs::create_dir(&watched).unwrap();
    fixture.pane("pane", &repository);
    fixture.ingest();
    fixture.coverage();
    assert!(fixture.worker.installed.contains(&watched));
    let moved = repository.join("moved");
    fs::rename(&watched, &moved).unwrap();
    fs::create_dir(&watched).unwrap();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[watched.clone(), moved],
    );
    assert!(!fixture.worker.installed.contains(&watched));
    assert!(fixture.worker.cache[&repository].rebuild);
    assert!(fixture.worker.cache[&repository].due.is_some());
}

#[test]
fn native_watch_discovers_new_ref_directories_then_observes_later_ref_updates() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-b", "main"]);
    fixture.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "--allow-empty",
        "-m",
        "initial",
    ]);
    fixture.git(&["switch", "-c", "feature"]);
    fs::write(fixture.repo.path().join("tracked"), "one\n").unwrap();
    fixture.git(&["add", "tracked"]);
    fixture.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "-m",
        "feature",
    ]);
    fixture.git(&["config", "branch.feature.gh-merge-base", "stack/base"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project.repository.as_ref().is_some_and(|repository| {
                repository.base_ref.as_deref() == Some("stack/base")
                    && repository.base_sha.is_none()
            })
    });
    fixture.git(&["branch", "stack/base", "main"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project.repository.as_ref().is_some_and(|repository| {
                repository
                    .committed
                    .is_some_and(|committed| committed.insertions == 1)
            })
    });
    fixture.git(&["update-ref", "refs/heads/stack/base", "HEAD"]);
    fixture.wait(|project| {
        project.state == ProjectState::Ready
            && project.repository.as_ref().is_some_and(|repository| {
                repository
                    .committed
                    .is_some_and(|committed| committed.files == 0)
            })
    });
}

#[test]
fn linked_head_and_main_private_refs_do_not_invalidate_other_worktrees() {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    let linked = fixture.linked(&repository);
    let other = fixture.repository("other");
    fixture.pane("main", &repository);
    fixture.pane("linked", &linked);
    fixture.pane("other", &other);
    fixture.ingest();
    fixture.coverage();
    let linked_git = fixture.worker.cache[&linked]
        .identity
        .as_ref()
        .unwrap()
        .git_dir
        .clone();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )),
        &[linked_git.join("HEAD")],
    );
    assert!(fixture.worker.cache[&linked].due.is_some());
    assert!(fixture.worker.cache[&linked].metadata_dirty);
    assert!(fixture.worker.cache[&repository].due.is_none());
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.refresh_dirty();

    fixture.git(
        &repository,
        &["update-ref", "refs/worktree/private", "HEAD"],
    );
    let private = repository.join(".git/refs/worktree");
    fixture.coverage();
    assert!(
        fixture.worker.cache[&repository]
            .directories
            .contains(&private)
    );
    assert!(!fixture.worker.cache[&linked].directories.contains(&private));
    fs::remove_dir_all(&private).unwrap();
    fixture.event(
        EventKind::Remove(notify::event::RemoveKind::Folder),
        std::slice::from_ref(&private),
    );
    assert!(fixture.worker.cache[&repository].due.is_some());
    assert!(fixture.worker.cache[&repository].metadata_dirty);
    assert!(fixture.worker.cache[&linked].due.is_none());
    assert!(fixture.worker.cache[&other].due.is_none());
    assert!(fixture.worker.rebind.is_empty());
    assert!(fixture.worker.cache.values().all(|entry| {
        entry
            .directories
            .iter()
            .all(|path| !path.starts_with(&private))
    }));
}

#[test]
fn same_path_repository_replacement_refreshes_head_and_branch_without_rechecking_other_repositories()
 {
    let mut fixture = Publication::new();
    let repository = fixture.repository("repository");
    fixture.git(&repository, &["commit", "--allow-empty", "-m", "original"]);
    let replacement = fixture.repository("replacement");
    fixture.git(&replacement, &["switch", "-c", "replacement"]);
    fixture.git(
        &replacement,
        &["commit", "--allow-empty", "-m", "replacement"],
    );
    let other = fixture.repository("other");
    fixture.pane("pane", &repository);
    fixture.pane("other", &other);
    fixture.ingest();
    fixture.coverage();
    let before = fixture.project("pane").repository.unwrap();
    let other_generation = fixture.worker.cache[&other].generation;

    let retired = fixture.root.path().join("retired");
    fs::rename(&repository, &retired).unwrap();
    fs::rename(&replacement, &repository).unwrap();
    fixture.event(
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )),
        &[repository.clone(), retired],
    );
    assert_eq!(fixture.worker.rebind, BTreeSet::from([repository.clone()]));
    assert!(fixture.worker.cache[&repository].metadata_dirty);
    assert!(fixture.worker.cache[&other].due.is_none());
    fixture.refresh_dirty();

    let project = fixture.project("pane");
    assert_eq!(project.state, ProjectState::Ready);
    let after = project.repository.unwrap();
    assert_eq!(after.root, before.root);
    assert_eq!(after.common_dir, before.common_dir);
    assert_eq!(after.git_dir, before.git_dir);
    assert_eq!(after.branch.as_deref(), Some("replacement"));
    assert_ne!(after.head_sha, before.head_sha);
    assert_eq!(fixture.worker.cache[&other].generation, other_generation);
    assert!(fixture.worker.cache[&other].due.is_none());
}
