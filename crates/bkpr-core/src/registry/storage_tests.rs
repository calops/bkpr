use super::*;

struct ResumeStorage(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ResumeStorage {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}

#[test]
fn blocked_metadata_write_does_not_block_pane_input_snapshots_or_committed_registry_reads() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "before write");
    let home = fixture.home("reader");
    let mut launch = spec(&home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"IFS= read -r line; printf '%s' "$line" > received; exec sleep 60"#.into(),
    ];
    let pane_id = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace_id.clone(),
            expected_revision: 1,
            name: None,
            spec: launch,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let view_id = view(registry, &pane_id, 80);
    let before = registry.snapshot().unwrap();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeStorage(Some(resume));
    let writing_registry = registry.clone();
    let writing_workspace = workspace_id.clone();
    let writing = std::thread::spawn(move || {
        writing_registry.request(Request::new(Operation::WorkspaceRename {
            workspace_id: writing_workspace,
            expected_revision: 2,
            name: "after write".into(),
        }))
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let controlling_registry = registry.clone();
    let reader_home = home.clone();
    let (progress_tx, progress_rx) = std::sync::mpsc::sync_channel(1);
    let controls = std::thread::spawn(move || {
        let result = (|| -> Result<Value> {
            controlling_registry.request(Request::new(Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Input {
                    view_id,
                    data: b"unblocked\n".to_vec(),
                },
            }))?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while fs::read(reader_home.join("received")).ok().as_deref()
                != Some(b"unblocked".as_slice())
            {
                ensure!(
                    Instant::now() < deadline,
                    "PTY did not receive input during metadata stall"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let snapshot = controlling_registry.request(Request::new(Operation::Pane {
                pane_id,
                request: PaneRequest::Inspect,
            }))?;
            let _: PaneSnapshot = serde_json::from_value(snapshot.into_json()?)?;
            controlling_registry
                .request(Request::new(Operation::Inspect))
                .and_then(Payload::into_json)
        })();
        let _ = progress_tx.send(result);
    });
    // Release before assertions/join so a regression fails rather than hanging cleanup.
    let progress = progress_rx.recv_timeout(Duration::from_secs(3));
    drop(release);
    let committed = writing.join().unwrap().unwrap().into_json().unwrap();
    controls.join().unwrap();
    let result = progress
        .expect("pane controls were blocked by registry durability work")
        .unwrap();
    let observed: RegistryResult = serde_json::from_value(result).unwrap();
    assert_eq!(
        observed.registry.revision, before.revision,
        "uncommitted metadata leaked to a reader"
    );
    assert_eq!(
        observed
            .registry
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .unwrap()
            .name,
        "before write"
    );
    let after: RegistryResult = serde_json::from_value(committed).unwrap();
    assert_eq!(
        after
            .registry
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .unwrap()
            .name,
        "after write"
    );
}

// LEGACY_COMPAT(next-major): remove with the one-shot v1 metadata upgrader.
fn legacy_metadata(root: &Path, foreign: bool) -> (String, String) {
    let registry = Registry::open(root).unwrap();
    let id = create(&registry, "saved");
    let (pane, _) = unavailable(&registry, &id, root);
    if foreign {
        create(&registry, "foreign");
    }
    let mut value = serde_json::to_value(&registry.inner.state.lock().unwrap().record).unwrap();
    drop(registry);
    value["version"] = json!(1);
    let workspaces = value["registry"]["workspaces"].as_array_mut().unwrap();
    for w in workspaces.iter_mut() {
        let windows = w.as_object_mut().unwrap().remove("windows").unwrap();
        w["layout"] = windows
            .as_array()
            .unwrap()
            .first()
            .map(|w| w["layout"].clone())
            .unwrap_or(Value::Null);
    }
    if foreign {
        workspaces[1]["layout"] = json!({"kind":"leaf","pane_id":pane});
    }
    for pane in value["registry"]["panes"].as_array_mut().unwrap() {
        pane.as_object_mut().unwrap().remove("window_id");
    }
    fs::write(
        root.join("registry.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    (id, pane)
}

#[test]
fn legacy_compat_next_major_upgrades_unambiguous_metadata_once_without_command_replay() {
    let root = private_tempdir();
    let (workspace, pane) = legacy_metadata(root.path(), false);
    let registry = Registry::open(root.path()).unwrap();
    let r = registry.snapshot().unwrap();
    assert_eq!(r.workspaces[0].windows[0].id, workspace);
    assert_eq!(r.panes[0].window_id.as_deref(), Some(workspace.as_str()));
    assert_eq!(
        r.workspaces[0].windows[0].layout,
        Some(LayoutNode::Leaf { pane_id: pane })
    );
    assert_eq!(r.panes[0].status, PaneStatus::Stopped);
    assert!(registry.inner.state.lock().unwrap().live.is_empty());
    let bytes = fs::read(root.path().join("registry.json")).unwrap();
    drop(registry);
    let registry = Registry::open(root.path()).unwrap();
    assert_eq!(fs::read(root.path().join("registry.json")).unwrap(), bytes);
    assert!(!registry.session_end_requested());
}

#[test]
fn legacy_compat_next_major_refuses_ambiguous_foreign_layout_without_overwriting_metadata() {
    let root = private_tempdir();
    legacy_metadata(root.path(), true);
    let before = fs::read(root.path().join("registry.json")).unwrap();
    assert!(Registry::open(root.path()).is_err());
    assert_eq!(fs::read(root.path().join("registry.json")).unwrap(), before);
}

// LEGACY_COMPAT(next-major): retired v1 receipts must not keep a workspace alive.
#[test]
fn review_regression_legacy_exited_receipt_does_not_become_owned_terminal() {
    let root = private_tempdir();
    let (workspace_id, saved_id) = legacy_metadata(root.path(), false);
    let path = root.path().join("registry.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut receipt = value["registry"]["panes"][0].clone();
    let receipt_id = Uuid::new_v4().to_string();
    receipt["id"] = json!(receipt_id);
    receipt["status"] = json!("exited");
    receipt["exit_code"] = json!(9);
    value["registry"]["panes"]
        .as_array_mut()
        .unwrap()
        .push(receipt);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let restored = registry.snapshot().unwrap();
    let receipt = restored.panes.iter().find(|p| p.id == receipt_id).unwrap();
    assert_eq!(receipt.exit_code, Some(9));
    let reopened = send(
        &registry,
        Operation::PaneReopen {
            workspace_id: workspace_id.clone(),
            window_id: restored.workspaces[0].windows[0].id.clone(),
            expected_revision: restored.workspaces[0].windows[0].revision,
            pane_id: saved_id,
            spec: spec(&root.path().join("reopened")),
        },
    );
    send(
        &registry,
        Operation::PaneTerminate {
            pane_id: reopened["pane_id"].as_str().unwrap().into(),
        },
    );
    let ended = registry.snapshot().unwrap();
    assert!(ended.workspaces.is_empty());
    assert!(registry.session_end_requested());
    let receipt = ended.panes.iter().find(|p| p.id == receipt_id).unwrap();
    assert!(receipt.workspace_id.is_none() && receipt.window_id.is_none());
    assert_eq!(receipt.exit_code, Some(9));
    registry.shutdown().unwrap();
}
