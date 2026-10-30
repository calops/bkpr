use super::*;

struct ResumeResize(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ResumeResize {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}
struct ReleaseClipboard {
    registry: Registry,
    pane_id: String,
    view_id: String,
    request_id: Option<String>,
}
impl ReleaseClipboard {
    fn release(&mut self) {
        if let Some(request_id) = self.request_id.take() {
            let _ = pane_request(
                &self.registry,
                &self.pane_id,
                PaneRequest::Clipboard {
                    view_id: self.view_id.clone(),
                    request_id,
                    response: ClipboardResponse::denied(),
                },
            );
        }
    }
}
impl Drop for ReleaseClipboard {
    fn drop(&mut self) {
        self.release();
    }
}
fn pane_request(registry: &Registry, pane_id: &str, request: PaneRequest) -> Result<Value> {
    registry.request(Request::new(Operation::Pane {
        pane_id: pane_id.into(),
        request,
    }))
}
fn policy_spec(cols: u16) -> ViewSpec {
    ViewSpec {
        size: size(cols),
        visible: true,
        interactive: true,
        input: false,
        clipboard: false,
    }
}
fn poll(registry: &Registry, pane_id: &str, view_id: &str) -> PaneSnapshot {
    serde_json::from_value(
        pane_request(
            registry,
            pane_id,
            PaneRequest::Poll {
                view_id: view_id.into(),
                after: 0,
                frame_after: 0,
                resources: Vec::new(),
                snapshot_after: 0,
                wait_ms: 0,
                row_deltas: false,
                scene_encoding: None,
            },
        )
        .unwrap(),
    )
    .unwrap()
}
fn geometry(registry: &Registry, pane_id: &str, view_id: &str) -> (u16, u16) {
    let snapshot = poll(registry, pane_id, view_id);
    (
        snapshot.size_owner.unwrap().size.cols,
        snapshot.frame.unwrap().size.cols,
    )
}
fn wait_file(path: &Path, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if fs::read(path).ok().as_deref() == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "child did not produce {:?}: {:?}",
            path,
            fs::read(path)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn assert_physical_size(registry: &Registry, pane_id: &str, owner: &str, home: &Path, cols: u16) {
    // The priority resize wake is consumed before this native command acknowledgement.
    pane_request(
        registry,
        pane_id,
        PaneRequest::Focus {
            view_id: owner.into(),
            focused: true,
        },
    )
    .unwrap();
    pane_request(
        registry,
        pane_id,
        PaneRequest::Input {
            view_id: owner.into(),
            data: b"stty size > actual-size\n".to_vec(),
        },
    )
    .unwrap();
    wait_file(&home.join("actual-size"), format!("24 {cols}\n").as_bytes());
    assert_eq!(geometry(registry, pane_id, owner), (cols, cols));
}
fn spawn_shell(registry: &Registry, home: &Path, argv: Vec<String>) -> String {
    let mut launch = spec(home);
    launch.argv = argv;
    send(
        registry,
        Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: launch,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn view_policy_acceptance_does_not_wait_for_clipboard_or_pending_native_input() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("clipboard-policy");
    let pane_id = spawn_shell(
        registry,
        &home,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            r"while [ ! -f query ]; do sleep 0.01; done; printf '\033]52;c;?\007'; exec /bin/sh"
                .into(),
        ],
    );
    let owner = view(registry, &pane_id, 80);
    let other_home = fixture.home("unrelated");
    let other = spawn_shell(registry, &other_home, vec!["/bin/sh".into()]);
    let other_owner = view(registry, &other, 80);
    fs::write(home.join("query"), b"query").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let pending = loop {
        if let Some(pending) = poll(registry, &pane_id, &owner).clipboard {
            break pending;
        }
        assert!(
            Instant::now() < deadline,
            "clipboard did not hold the native worker"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut release = ReleaseClipboard {
        registry: registry.clone(),
        pane_id: pane_id.clone(),
        view_id: owner.clone(),
        request_id: Some(pending.request_id),
    };
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let input_registry = registry.clone();
    let input_pane = pane_id.clone();
    let input_owner = owner.clone();
    let input = std::thread::spawn(move || {
        pane_request(
            &input_registry,
            &input_pane,
            PaneRequest::Input {
                view_id: input_owner,
                data: b"printf accepted > accepted-input\n".to_vec(),
            },
        )
    });
    // Scheduling barrier only: a real authorized input now waits behind the callback,
    // holding the native-command gate. Policy acceptance must not share that gate.
    let deadline = Instant::now() + Duration::from_secs(1);
    while pane.commands.is_empty() {
        assert!(
            Instant::now() < deadline,
            "input never entered the native queue"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let policy_registry = registry.clone();
    let policy_pane = pane_id.clone();
    let policy_owner = owner.clone();
    let (progress_tx, progress_rx) = std::sync::mpsc::sync_channel(1);
    let policy = std::thread::spawn(move || {
        let result = (|| -> Result<Vec<(u16, u16)>> {
            let attached = pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Attach {
                    spec: policy_spec(120),
                },
            )?;
            let temporary = attached["view_id"].as_str().unwrap().to_owned();
            let mut observed = vec![geometry(&policy_registry, &policy_pane, &policy_owner)];
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Update {
                    view_id: temporary.clone(),
                    spec: policy_spec(140),
                },
            )?;
            observed.push(geometry(&policy_registry, &policy_pane, &policy_owner));
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Pin {
                    view_id: policy_owner.clone(),
                    size: Some(size(100)),
                },
            )?;
            observed.push(geometry(&policy_registry, &policy_pane, &policy_owner));
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Update {
                    view_id: temporary.clone(),
                    spec: policy_spec(160),
                },
            )?;
            observed.push(geometry(&policy_registry, &policy_pane, &policy_owner));
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Detach { view_id: temporary },
            )?;
            observed.push(geometry(&policy_registry, &policy_pane, &policy_owner));
            Ok(observed)
        })();
        let _ = progress_tx.send(result);
    });
    // Unrelated native input must also progress before the blocked pane is released.
    pane_request(
        registry,
        &other,
        PaneRequest::Input {
            view_id: other_owner,
            data: b"printf other > progressed\n".to_vec(),
        },
    )
    .unwrap();
    wait_file(&other_home.join("progressed"), b"other");
    let progress = progress_rx.recv_timeout(Duration::from_secs(1));
    release.release();
    policy.join().unwrap();
    let accepted_input = input.join().unwrap().unwrap();
    assert_eq!(accepted_input["accepted"], true);
    let observed = progress
        .expect("view policy waited for native execution")
        .unwrap();
    assert_eq!(
        observed,
        vec![(120, 80), (140, 80), (100, 80), (100, 80), (100, 80)],
        "policy results must expose desired geometry separately from the blocked frame"
    );
    wait_file(&home.join("accepted-input"), b"accepted");
    assert_physical_size(registry, &pane_id, &owner, &home, 100);
    assert_eq!(poll(registry, &pane_id, &owner).views.len(), 1);
}

#[test]
fn policy_acceptance_during_in_flight_resize_converges_to_latest_authority() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("in-flight-policy");
    let pane_id = spawn_shell(registry, &home, vec!["/bin/sh".into()]);
    let owner = view(registry, &pane_id, 80);
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *pane.shared.resize_barrier.lock().unwrap() = Some(Arc::new(crate::ResizeBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeResize(Some(resume));
    let attaching_registry = registry.clone();
    let attaching_pane = pane_id.clone();
    let (attached_tx, attached_rx) = std::sync::mpsc::sync_channel(1);
    let attaching = std::thread::spawn(move || {
        let _ = attached_tx.send(pane_request(
            &attaching_registry,
            &attaching_pane,
            PaneRequest::Attach {
                spec: policy_spec(120),
            },
        ));
    });
    let captured = entered_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("native resize did not enter controlled boundary");
    assert_eq!(captured.cols, 120);
    let admission = attached_rx.recv_timeout(Duration::from_secs(1));
    if admission.is_err() {
        drop(release);
        attaching.join().unwrap();
        panic!("Attach waited for native resize instead of returning policy acceptance");
    }
    let admitted = admission.unwrap().unwrap();
    attaching.join().unwrap();
    let temporary = admitted["view_id"].as_str().unwrap().to_owned();
    let policy_registry = registry.clone();
    let policy_pane = pane_id.clone();
    let policy_owner = owner.clone();
    let (progress_tx, progress_rx) = std::sync::mpsc::sync_channel(1);
    let policy = std::thread::spawn(move || {
        let result = (|| -> Result<(u16, u16)> {
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Update {
                    view_id: temporary.clone(),
                    spec: policy_spec(150),
                },
            )?;
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Pin {
                    view_id: policy_owner.clone(),
                    size: Some(size(90)),
                },
            )?;
            pane_request(
                &policy_registry,
                &policy_pane,
                PaneRequest::Detach { view_id: temporary },
            )?;
            Ok(geometry(&policy_registry, &policy_pane, &policy_owner))
        })();
        let _ = progress_tx.send(result);
    });
    let progress = progress_rx.recv_timeout(Duration::from_secs(1));
    // The native operation applies its already-captured120, then the pending coalesced
    // wake must reconcile latest90 before the following FIFO control acknowledgement.
    drop(release);
    policy.join().unwrap();
    assert_eq!(
        progress
            .expect("policy update waited for in-flight native resize")
            .unwrap(),
        (90, 80)
    );
    assert_physical_size(registry, &pane_id, &owner, &home, 90);
    assert_eq!(poll(registry, &pane_id, &owner).views.len(), 1);
}

#[test]
fn enhanced_paste_never_combines_permissions_from_different_policy_versions() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("compound-grant");
    let pane_id = spawn_shell(registry, &home, vec!["/bin/sh".into()]);
    let mut input_only = policy_spec(80);
    input_only.input = true;
    let view_id = pane_request(
        registry,
        &pane_id,
        PaneRequest::Attach {
            spec: input_only.clone(),
        },
    )
    .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut clipboard_only = policy_spec(80);
    clipboard_only.clipboard = true;
    let start = Arc::new(std::sync::Barrier::new(2));
    let changing_registry = registry.clone();
    let changing_pane = pane_id.clone();
    let changing_view = view_id.clone();
    let changing_start = start.clone();
    let changing = std::thread::spawn(move || {
        changing_start.wait();
        for _ in 0..2000 {
            pane_request(
                &changing_registry,
                &changing_pane,
                PaneRequest::Update {
                    view_id: changing_view.clone(),
                    spec: clipboard_only.clone(),
                },
            )
            .unwrap();
            std::thread::yield_now();
            pane_request(
                &changing_registry,
                &changing_pane,
                PaneRequest::Update {
                    view_id: changing_view.clone(),
                    spec: input_only.clone(),
                },
            )
            .unwrap();
            std::thread::yield_now();
        }
    });
    start.wait();
    let mut accepted = 0;
    for _ in 0..1000 {
        let result = pane_request(
            registry,
            &pane_id,
            PaneRequest::PasteContents {
                view_id: view_id.clone(),
                location: ClipboardLocation::Standard,
                allow_unsafe: false,
                contents: vec![ClipboardContent {
                    mime: "text/plain".into(),
                    data: b"printf forbidden > forbidden\n".to_vec(),
                }],
            },
        );
        if result.is_ok() {
            accepted += 1;
        }
        std::thread::yield_now();
    }
    changing.join().unwrap();
    // Full authority is granted only after all adversarial paste attempts ended.
    let mut complete = policy_spec(80);
    complete.input = true;
    complete.clipboard = true;
    pane_request(
        registry,
        &pane_id,
        PaneRequest::Update {
            view_id: view_id.clone(),
            spec: complete,
        },
    )
    .unwrap();
    pane_request(
        registry,
        &pane_id,
        PaneRequest::Input {
            view_id,
            data: b"printf done > done\n".to_vec(),
        },
    )
    .unwrap();
    wait_file(&home.join("done"), b"done");
    assert_eq!(
        accepted, 0,
        "paste accepted although no complete grant ever existed"
    );
    assert!(
        !home.join("forbidden").exists(),
        "an unauthorized paste reached the actual shell"
    );
}
