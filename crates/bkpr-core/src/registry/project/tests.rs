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
