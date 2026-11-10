use super::*;
use std::time::Instant;

fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn size(cols: u16) -> Size {
    Size {
        cols,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    }
}
fn send(registry: &Registry, op: Operation) -> Value {
    registry
        .request(Request::new(op))
        .unwrap()
        .into_json()
        .unwrap()
}
fn create(registry: &Registry, name: &str) -> String {
    send(
        registry,
        Operation::WorkspaceCreate {
            name: Some(name.into()),
            initial: None,
        },
    )["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn spec(home: &Path) -> PaneSpawnSpec {
    PaneSpawnSpec {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf READY; exec sleep 60".into(),
        ],
        cwd: home.to_owned(),
        home: home.to_owned(),
        size: size(80),
        title: Some("shell".into()),
        isolated_environment: true,
    }
}
// Test consumers choose their placement explicitly; the server has no append policy.
fn split_new_pane(layout: Option<&LayoutNode>) -> SpawnLayoutNode {
    let new = SpawnLayoutNode::New {
        reference: "pane".into(),
    };
    match layout {
        None => new,
        Some(first) => SpawnLayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(existing_spawn_layout(first)),
            second: Box::new(new),
        },
    }
}
fn spawn(
    registry: &Registry,
    home: &Path,
    workspace_id: Option<String>,
    expected_revision: Option<u64>,
) -> String {
    let operation = if let Some(workspace_id) = workspace_id {
        let snapshot = registry.snapshot().unwrap();
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .unwrap();
        if let Some(window) = workspace.windows.first() {
            Operation::PaneSpawn {
                workspace_id: Some(workspace_id),
                window_id: Some(window.id.clone()),
                expected_revision,
                panes: vec![PaneSpawnEntry {
                    reference: "pane".into(),
                    spec: spec(home),
                }],
                layout: Some(split_new_pane(window.layout.as_ref())),
            }
        } else {
            Operation::WindowCreate {
                workspace_id,
                expected_revision: expected_revision.unwrap(),
                name: None,
                spec: spec(home),
            }
        }
    } else {
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision,
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: spec(home),
            }],
            layout: None,
        }
    };
    send(registry, operation)["pane_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn view(registry: &Registry, pane_id: &str, cols: u16) -> String {
    send(
        registry,
        Operation::Pane {
            pane_id: pane_id.into(),
            request: PaneRequest::Attach {
                spec: ViewSpec {
                    size: size(cols),
                    visible: true,
                    interactive: true,
                    input: true,
                    clipboard: true,
                },
            },
        },
    )["view_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn snapshot(registry: &Registry, pane: &str) -> Value {
    send(
        registry,
        Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Inspect,
        },
    )
}
struct Fixture {
    registry: Registry,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = private_tempdir();
        let registry = Registry::open(root.path()).unwrap();
        Self {
            registry,
            _root: root,
        }
    }
    fn home(&self, name: &str) -> PathBuf {
        self._root.path().join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.registry.shutdown();
    }
}

// An unavailable execution fixture models durable metadata, never command replay.
fn unavailable(registry: &Registry, workspace_id: &str, home: &Path) -> (String, String) {
    let pane_id = Uuid::new_v4().to_string();
    let window_id = Uuid::new_v4().to_string();
    registry
        .mutate(|r| {
            r.panes.push(PaneInfo {
                id: pane_id.clone(),
                workspace_id: Some(workspace_id.into()),
                window_id: Some(window_id.clone()),
                title: "unavailable".into(),
                status: PaneStatus::Stopped,
                exit_code: Some(7),
                cwd: home.into(),
                home: home.into(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                project: None,
                isolated_environment: true,
                harness_status_changed_at_ms: None,
            });
            let workspace = r
                .workspaces
                .iter_mut()
                .find(|w| w.id == workspace_id)
                .unwrap();
            workspace.windows.push(Window {
                id: window_id.clone(),
                name: "Saved".into(),
                revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: pane_id.clone(),
                }),
            });
            workspace.revision += 1;
            Ok(())
        })
        .unwrap();
    (pane_id, window_id)
}

#[test]
fn workspace_revisions_fail_closed_and_metadata_survives_restart() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let id = create(&registry, "alpha");
    send(
        &registry,
        Operation::WorkspaceRename {
            workspace_id: id.clone(),
            expected_revision: 1,
            name: "renamed".into(),
        },
    );
    let before = fs::read(root.path().join("registry.json")).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceClose {
                workspace_id: id.clone(),
                expected_revision: 1
            }))
            .is_err()
    );
    assert_eq!(fs::read(root.path().join("registry.json")).unwrap(), before);
    drop(registry);
    let restored = Registry::open(root.path()).unwrap();
    assert!(!restored.is_fresh());
    let r = restored.snapshot().unwrap();
    assert_eq!(r.workspaces[0].id, id);
    assert_eq!(r.workspaces[0].name, "renamed");
    assert_eq!(r.workspaces[0].revision, 2);
    assert!(!restored.session_end_requested());
}
#[test]
fn state_root_is_exclusively_owned_and_future_records_are_never_overwritten() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    assert!(Registry::open(root.path()).is_err());
    drop(registry);
    let future = br#"{"version":999,"registry":{"revision":0,"workspaces":[],"panes":[]}}"#;
    fs::write(root.path().join("registry.json"), future).unwrap();
    assert!(Registry::open(root.path()).is_err());
    assert_eq!(fs::read(root.path().join("registry.json")).unwrap(), future);
}
#[test]
fn restart_preserves_window_membership_without_content_or_token_recovery() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace_id = create(&registry, "saved");
    let (pane_id, window_id) = unavailable(&registry, &workspace_id, root.path());
    registry
        .mutate(|r| {
            r.panes[0].status = PaneStatus::Running;
            Ok(())
        })
        .unwrap();
    drop(registry);
    let restored = Registry::open(root.path()).unwrap();
    let r = restored.snapshot().unwrap();
    assert_eq!(r.panes[0].status, PaneStatus::Stopped);
    assert_eq!(r.panes[0].window_id.as_deref(), Some(window_id.as_str()));
    assert_eq!(
        r.workspaces[0].windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: pane_id.clone()
        })
    );
    assert!(
        restored
            .request(Request::new(Operation::Pane {
                pane_id,
                request: PaneRequest::Inspect
            }))
            .is_err()
    );
    let value: Value =
        serde_json::from_slice(&fs::read(root.path().join("registry.json")).unwrap()).unwrap();
    for forbidden in [
        "frame",
        "events",
        "views",
        "clipboard",
        "argv",
        "token",
        "pid",
    ] {
        assert!(
            value["registry"]["panes"][0].get(forbidden).is_none(),
            "persisted {forbidden}"
        );
    }
}
#[test]
fn malformed_membership_and_unknown_versions_do_not_mutate_registry() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "layout");
    let (_, window_id) = unavailable(registry, &id, fixture._root.path());
    let before = registry.snapshot().unwrap().revision;
    assert!(
        registry
            .request(Request::new(Operation::LayoutUpdate {
                workspace_id: id,
                window_id,
                expected_revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: Uuid::new_v4().to_string()
                })
            }))
            .is_err()
    );
    assert!(
        registry
            .request(Request {
                version: PROTOCOL_VERSION - 1,
                operation: Operation::WorkspaceCreate {
                    name: None,
                    initial: None
                }
            })
            .is_err()
    );
    assert_eq!(registry.snapshot().unwrap().revision, before);
}
#[test]
fn persistence_failure_poisoning_never_claims_durable_success() {
    let fixture = Fixture::new();
    let path = fixture._root.path().join("registry.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        fixture
            .registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: None
            }))
            .is_err()
    );
    assert!(
        fixture
            .registry
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("persistence failed")
    );
    assert!(!fixture.registry.session_end_requested());
}
#[test]
fn independent_windows_reject_foreign_membership_and_layout_removal_is_not_close() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "A");
    let b = create(registry, "B");
    let one = spawn(registry, &fixture.home("one"), Some(a.clone()), Some(1));
    let two = spawn(registry, &fixture.home("two"), Some(b.clone()), Some(1));
    let first_view = view(registry, &one, 91);
    let second_view = view(registry, &two, 72);
    assert_eq!(snapshot(registry, &one)["size_owner"]["size"]["cols"], 91);
    assert_eq!(snapshot(registry, &two)["size_owner"]["size"]["cols"], 72);
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: two.clone(),
                request: PaneRequest::Input {
                    view_id: first_view.clone(),
                    data: b"x".to_vec()
                }
            }))
            .is_err()
    );
    let r = registry.snapshot().unwrap();
    let aw = r.workspaces.iter().find(|w| w.id == a).unwrap();
    let bw = r.workspaces.iter().find(|w| w.id == b).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::LayoutUpdate {
                workspace_id: b.clone(),
                window_id: bw.windows[0].id.clone(),
                expected_revision: bw.windows[0].revision,
                layout: Some(LayoutNode::Leaf {
                    pane_id: one.clone()
                })
            }))
            .is_err()
    );
    send(
        registry,
        Operation::Pane {
            pane_id: one.clone(),
            request: PaneRequest::Detach {
                view_id: first_view,
            },
        },
    );
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: a.clone(),
            window_id: aw.windows[0].id.clone(),
            expected_revision: aw.windows[0].revision,
            layout: None,
        },
    );
    assert_eq!(snapshot(registry, &one)["lifecycle"], "running");
    assert!(!registry.session_end_requested());
    let revision = registry
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|w| w.id == a)
        .unwrap()
        .revision;
    send(
        registry,
        Operation::WorkspaceClose {
            workspace_id: a,
            expected_revision: revision,
        },
    );
    assert_eq!(snapshot(registry, &one)["lifecycle"], "terminated");
    assert_eq!(snapshot(registry, &two)["lifecycle"], "running");
    send(
        registry,
        Operation::Pane {
            pane_id: two,
            request: PaneRequest::Input {
                view_id: second_view,
                data: Vec::new(),
            },
        },
    );
}
#[test]
fn pane_wait_and_blocked_pane_serialization_do_not_lock_registry_controls() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "responsive");
    let pane_id = spawn(registry, &fixture.home("pane"), Some(id.clone()), Some(1));
    let view_id = view(registry, &pane_id, 80);
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let blocked = pane.shared.requests.lock().unwrap();
    let other = registry.clone();
    let revision = snapshot(registry, &pane_id)["snapshot_revision"]
        .as_u64()
        .unwrap();
    let waiting = std::thread::spawn(move || {
        other.request(Request::new(Operation::Pane {
            pane_id,
            request: PaneRequest::Poll {
                view_id,
                after: 0,
                frame_after: 0,
                resources: Vec::new(),
                snapshot_after: revision,
                wait_ms: 1000,
                row_deltas: false,
                scene_encoding: None,
            },
        }))
    });
    let start = Instant::now();
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: id,
            expected_revision: 2,
            name: "still responsive".into(),
        },
    );
    assert!(start.elapsed() < Duration::from_millis(500));
    waiting.join().unwrap().unwrap();
    drop(blocked);
}
#[test]
fn normal_home_and_environment_are_preserved_and_initial_cwd_is_recorded() {
    let fixture = Fixture::new();
    let home = fixture.home("real-home");
    let cwd = fixture.home("working-directory");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&cwd).unwrap();
    fs::write(home.join("existing-profile"), b"do not touch").unwrap();
    let mut launch = spec(&home);
    launch.cwd = cwd.clone();
    launch.isolated_environment = false;
    launch.argv = vec!["/bin/sh".into(), "-c".into(), r#"pwd > actual-cwd; printf '%s' "$HOME" > actual-home; printf '%s' "$PATH" > actual-path; exec sleep 60"#.into()];
    let id = send(
        &fixture.registry,
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: launch,
            }],
            layout: None,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !cwd.join("actual-path").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read(home.join("existing-profile")).unwrap(),
        b"do not touch"
    );
    assert!(!home.join(".bkpr-native-home").exists());
    assert_eq!(
        fs::read_to_string(cwd.join("actual-home")).unwrap(),
        home.to_string_lossy()
    );
    assert_eq!(
        fs::read_to_string(cwd.join("actual-cwd")).unwrap().trim(),
        cwd.to_string_lossy()
    );
    assert_eq!(
        fs::read_to_string(cwd.join("actual-path")).unwrap(),
        std::env::var("PATH").unwrap()
    );
    assert_eq!(
        fixture
            .registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .cwd,
        cwd
    );
}

#[path = "automatic_tests.rs"]
mod automatic_tests;
#[path = "clipboard_tests.rs"]
mod clipboard_tests;
#[path = "completion_tests.rs"]
mod completion_tests;
#[cfg(target_os = "linux")]
#[path = "context_exec_tests.rs"]
mod context_exec_tests;
#[path = "forget_tests.rs"]
mod forget_tests;
#[path = "recovery_tests.rs"]
mod recovery_tests;
#[path = "reopen_tests.rs"]
mod reopen_tests;
#[path = "resize_tests.rs"]
mod resize_tests;
#[path = "spawn_tests.rs"]
mod spawn_tests;
#[path = "storage_tests.rs"]
mod storage_tests;
#[path = "title_tests.rs"]
mod title_tests;
#[path = "view_lifecycle_tests.rs"]
mod view_lifecycle_tests;
