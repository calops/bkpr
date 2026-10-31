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
    registry.request(Request::new(op)).unwrap()
}
fn create(registry: &Registry, name: &str) -> String {
    send(registry, Operation::WorkspaceCreate { name: name.into() })["workspace_id"]
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
fn spawn(
    registry: &Registry,
    home: &Path,
    workspace_id: Option<String>,
    expected_revision: Option<u64>,
) -> String {
    send(
        registry,
        Operation::PaneSpawn {
            workspace_id,
            expected_revision,
            spec: spec(home),
        },
    )["pane_id"]
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

#[test]
fn workspace_revisions_fail_closed_and_metadata_survives_restart() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    assert!(registry.is_fresh());
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
            .request(Request::new(Operation::WorkspaceRemove {
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
fn restart_marks_lost_executions_stopped_without_content_or_token_recovery() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let pane_id = Uuid::new_v4().to_string();
    let workspace_id = create(&registry, "saved");
    registry
        .mutate(|r| {
            r.panes.push(PaneInfo {
                id: pane_id.clone(),
                workspace_id: Some(workspace_id.clone()),
                title: "recorded".into(),
                status: PaneStatus::Running,
                exit_code: None,
                cwd: root.path().into(),
                home: root.path().into(),
                isolated_environment: false,
            });
            r.workspaces[0].layout = Some(LayoutNode::Leaf {
                pane_id: pane_id.clone(),
            });
            Ok(())
        })
        .unwrap();
    drop(registry);
    let restored = Registry::open(root.path()).unwrap();
    let r = restored.snapshot().unwrap();
    assert_eq!(r.panes[0].status, PaneStatus::Stopped);
    assert_eq!(r.panes[0].id, pane_id);
    assert_eq!(r.workspaces[0].id, workspace_id);
    assert!(
        restored
            .request(Request::new(Operation::Pane {
                pane_id,
                request: PaneRequest::Inspect
            }))
            .unwrap_err()
            .to_string()
            .contains("not recovered")
    );
    let value: Value =
        serde_json::from_slice(&fs::read(root.path().join("registry.json")).unwrap()).unwrap();
    let info = &value["registry"]["panes"][0];
    for forbidden in [
        "frame",
        "events",
        "views",
        "clipboard",
        "argv",
        "token",
        "pid",
    ] {
        assert!(info.get(forbidden).is_none(), "persisted {forbidden}");
    }
}

#[test]
fn malformed_layouts_and_unknown_protocol_versions_do_not_mutate_registry() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "layout");
    let before = registry.snapshot().unwrap().revision;
    assert!(
        registry
            .request(Request::new(Operation::LayoutUpdate {
                workspace_id: id,
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
                version: PROTOCOL_VERSION + 1,
                operation: Operation::WorkspaceCreate { name: "bad".into() }
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
                name: "must not succeed".into()
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
}

#[test]
fn independent_panes_keep_authority_and_survive_layout_workspace_and_view_removal() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "A");
    let b = create(registry, "B");
    let one = spawn(registry, &fixture.home("one"), Some(a.clone()), Some(1));
    let two = spawn(registry, &fixture.home("two"), Some(b.clone()), Some(1));
    assert_ne!(one, two);
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
            workspace_id: b.clone(),
            expected_revision: 2,
            layout: Some(LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 400,
                first: Box::new(LayoutNode::Leaf {
                    pane_id: two.clone(),
                }),
                second: Box::new(LayoutNode::Leaf {
                    pane_id: one.clone(),
                }),
            }),
        },
    );
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == one)
            .unwrap()
            .workspace_id
            .as_deref(),
        Some(a.as_str())
    );
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: a.clone(),
            expected_revision: 2,
            layout: None,
        },
    );
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == one)
            .unwrap()
            .workspace_id
            .as_deref(),
        Some(a.as_str())
    );
    send(
        registry,
        Operation::WorkspaceRemove {
            workspace_id: a,
            expected_revision: 3,
        },
    );
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == one)
            .unwrap()
            .workspace_id,
        None
    );
    assert_eq!(snapshot(registry, &one)["lifecycle"], "running");
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: one.clone(),
        },
    );
    assert_eq!(snapshot(registry, &one)["lifecycle"], "terminated");
    assert_eq!(snapshot(registry, &two)["lifecycle"], "running");
    send(
        registry,
        Operation::Pane {
            pane_id: two.clone(),
            request: PaneRequest::Input {
                view_id: second_view,
                data: Vec::new(),
            },
        },
    );
    let r = registry.snapshot().unwrap();
    assert_eq!(
        r.panes.iter().find(|p| p.id == one).unwrap().status,
        PaneStatus::Exited
    );
    assert_eq!(
        r.panes.iter().find(|p| p.id == two).unwrap().status,
        PaneStatus::Running
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
    let _blocked_runtime = pane.shared.requests.lock().unwrap();
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
    drop(_blocked_runtime);
}

#[test]
fn natural_exit_wakes_registry_wait_and_execution_metadata_remains() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let mut launch = spec(&fixture.home("short"));
    launch.argv = vec!["/bin/sh".into(), "-c".into(), "exit 7".into()];
    let pane_id = send(
        registry,
        Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: launch,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = registry.snapshot().unwrap();
        if let Some(pane) = snapshot
            .panes
            .iter()
            .find(|p| p.id == pane_id && p.status == PaneStatus::Exited)
        {
            assert_eq!(pane.exit_code, Some(7));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "natural exit did not update registry"
        );
        send(
            registry,
            Operation::Wait {
                after: snapshot.revision,
                wait_ms: 1000,
            },
        );
    }
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
    launch.argv = vec!["/bin/sh".into(), "-c".into(),
        r#"pwd > actual-cwd; printf '%s' "$HOME" > actual-home; printf '%s' "$PATH" > actual-path; exec sleep 60"#.into()];
    let id = send(
        &fixture.registry,
        Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: launch,
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

#[test]
fn restarted_exited_pane_is_stopped_with_historical_exit_code_but_no_scene() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace_id = create(&registry, "exited layout");
    let pane_id = Uuid::new_v4().to_string();
    registry
        .mutate(|r| {
            r.panes.push(PaneInfo {
                id: pane_id.clone(),
                workspace_id: Some(workspace_id.clone()),
                title: "finished".into(),
                status: PaneStatus::Exited,
                exit_code: Some(7),
                cwd: root.path().into(),
                home: root.path().into(),
                isolated_environment: false,
            });
            r.workspaces[0].layout = Some(LayoutNode::Leaf {
                pane_id: pane_id.clone(),
            });
            Ok(())
        })
        .unwrap();
    drop(registry);
    let restored = Registry::open(root.path()).unwrap();
    let result = send(&restored, Operation::Inspect);
    let result: RegistryResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.registry.panes[0].status, PaneStatus::Stopped);
    assert_eq!(result.registry.panes[0].exit_code, Some(7));
    assert_eq!(
        result.registry.workspaces[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: pane_id.clone()
        })
    );
    assert!(
        restored
            .request(Request::new(Operation::Pane {
                pane_id,
                request: PaneRequest::Attach {
                    spec: ViewSpec {
                        size: size(80),
                        interactive: true,
                        visible: true,
                        input: false,
                        clipboard: false
                    }
                }
            }))
            .is_err()
    );
}

#[path = "clipboard_tests.rs"]
mod clipboard_tests;

#[path = "spawn_tests.rs"]
mod spawn_tests;

#[path = "storage_tests.rs"]
mod storage_tests;

#[path = "forget_tests.rs"]
mod forget_tests;

#[path = "view_budget_tests.rs"]
mod view_budget_tests;

#[path = "resize_tests.rs"]
mod resize_tests;

#[path = "completion_tests.rs"]
mod completion_tests;
