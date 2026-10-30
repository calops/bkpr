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
        Operation::PaneSpawn {
            workspace_id: Some(workspace_id.clone()),
            expected_revision: Some(1),
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
            let _: PaneSnapshot = serde_json::from_value(snapshot)?;
            controlling_registry.request(Request::new(Operation::Inspect))
        })();
        let _ = progress_tx.send(result);
    });
    // Release before assertions/join so a regression fails rather than hanging cleanup.
    let progress = progress_rx.recv_timeout(Duration::from_secs(3));
    drop(release);
    let committed = writing.join().unwrap().unwrap();
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
