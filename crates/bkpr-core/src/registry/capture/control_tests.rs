use super::*;
use bkpr_protocol::control::{ControlAction, resolve};

struct ControlFixture {
    fixture: Fixture,
    pane: String,
    grant: CaptureGrant,
    peer: Peer,
    workspace: String,
    window: String,
    foreign: String,
}
impl ControlFixture {
    fn new() -> Self {
        let fixture = Fixture::new();
        fixture
            .registry
            .configure_harnesses(&fixture.socket, &std::env::current_exe().unwrap())
            .unwrap();
        // A stable native executable blocked in shell builtins: no fork/exec
        // classifier races and no real provider, external account or HOME.
        let (pane, grant, peer) = seed_launch_command(
            &fixture,
            HarnessKind::Claude,
            "printf 'READY\\346\\227\\245\\346\\234\\254\\n'; printf ready > control-ready; while IFS= read -r line; do printf '%s' \"$line\" > control-input; done",
        );
        await_file(&fixture.home.join("control-ready"));
        let (workspace, windows) = own_windows(&fixture.registry, std::slice::from_ref(&pane));
        let launch =
            fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
        fixture
            .registry
            .agent_register(&launch, &fixture.home)
            .unwrap();
        let foreign = fixture
            .registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: Some("foreign-control".into()),
                initial: Some(control_spec(
                    &fixture.home,
                    "while IFS= read -r line; do :; done",
                )),
            }))
            .unwrap()
            .into_json()
            .unwrap()["pane_id"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            fixture,
            pane,
            grant,
            peer,
            workspace,
            window: windows[0].clone(),
            foreign,
        }
    }
    fn raw(&self, operation: Operation, scope: AgentScope) -> Result<Value> {
        control_request(
            &self.fixture.registry,
            &self.grant,
            &self.peer,
            operation,
            scope,
        )
    }
    fn call(&self, operation: &str, arguments: Value, scope: AgentScope) -> Result<Value> {
        let ControlAction::Native(operation) = resolve(operation, arguments)? else {
            bail!("test expected native operation")
        };
        self.raw(operation, scope)
    }
    fn batch(&self, expected_revision: u64, layout: SpawnLayoutNode, script: &str) -> Operation {
        Operation::PaneSpawn {
            workspace_id: Some(self.workspace.clone()),
            window_id: Some(self.window.clone()),
            expected_revision: Some(expected_revision),
            panes: vec![PaneSpawnEntry {
                reference: "new".into(),
                spec: control_spec(&self.fixture.home, script),
            }],
            layout: Some(layout),
        }
    }
    fn layout(&self) -> SpawnLayoutNode {
        SpawnLayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(SpawnLayoutNode::Existing {
                pane_id: self.pane.clone(),
            }),
            second: Box::new(SpawnLayoutNode::New {
                reference: "new".into(),
            }),
        }
    }
}
fn control_spec(home: &Path, script: &str) -> PaneSpawnSpec {
    PaneSpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: home.into(),
        home: home.into(),
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        title: None,
        isolated_environment: true,
    }
}
fn control_request(
    registry: &Registry,
    grant: &CaptureGrant,
    peer: &Peer,
    operation: Operation,
    scope: AgentScope,
) -> Result<Value> {
    registry
        .request_authenticated(
            Request::new(Operation::Agent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                request: AgentRequest::Control {
                    request: Box::new(operation),
                    scope,
                },
            }),
            peer,
        )?
        .into_json()
}
fn human_view(f: &ControlFixture) -> String {
    f.fixture
        .registry
        .request(Request::new(Operation::Pane {
            pane_id: f.pane.clone(),
            request: PaneRequest::Attach {
                spec: ViewSpec {
                    size: Size {
                        cols: 91,
                        rows: 31,
                        cell_width: 8,
                        cell_height: 16,
                    },
                    visible: true,
                    interactive: true,
                    input: true,
                    clipboard: true,
                },
            },
        }))
        .unwrap()
        .into_json()
        .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn authenticated_control_filters_every_registry_reply_and_fences_foreign_ids() {
    let f = ControlFixture::new();
    let own = f
        .call("session.inspect", json!({}), AgentScope::Workspace)
        .unwrap();
    assert_eq!(own["registry"]["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(own["registry"]["panes"].as_array().unwrap().len(), 1);
    assert_eq!(own["registry"]["panes"][0]["id"], f.pane);
    let all = f
        .call("session.inspect", json!({}), AgentScope::Session)
        .unwrap();
    assert_eq!(all["registry"]["workspaces"].as_array().unwrap().len(), 2);
    for (operation, arguments) in [
        (
            "pane.rename",
            json!({"pane_id":f.foreign,"title":"forbidden"}),
        ),
        ("pane.close", json!({"pane_id":f.foreign})),
        ("pane.read", json!({"pane_id":f.foreign})),
        (
            "input.text",
            json!({"pane_id":f.foreign,"text":"forbidden"}),
        ),
        ("workspace.create", json!({"name":"forbidden"})),
    ] {
        assert!(
            f.call(operation, arguments, AgentScope::Workspace).is_err(),
            "{operation}"
        );
    }
    assert!(
        f.raw(
            Operation::PaneSpawn {
                workspace_id: None,
                window_id: None,
                expected_revision: None,
                panes: vec![PaneSpawnEntry {
                    reference: "new".into(),
                    spec: control_spec(&f.fixture.home, "printf bad > forbidden-start")
                }],
                layout: None,
            },
            AgentScope::Workspace
        )
        .is_err()
    );
    let rename = f
        .call(
            "pane.rename",
            json!({"pane_id":f.pane,"title":"controlled"}),
            AgentScope::Workspace,
        )
        .unwrap();
    assert_eq!(rename["registry"]["panes"].as_array().unwrap().len(), 1);
    assert_eq!(rename["registry"]["panes"][0]["title"], "controlled");
    assert!(
        f.call("session.inspect", json!({}), AgentScope::Repository)
            .is_err()
    );
    for operation in [
        Operation::SessionInfo,
        Operation::Shutdown,
        Operation::Wait {
            after: 0,
            wait_ms: 0,
        },
    ] {
        assert!(f.raw(operation, AgentScope::Session).is_err());
    }
    assert!(!f.fixture.home.join("forbidden-start").exists());
    assert!(f.fixture.snapshot(&f.foreign).views.is_empty());
}

#[test]
fn authenticated_control_batch_checks_all_existing_refs_and_stale_revision_before_exec() {
    let f = ControlFixture::new();
    let before = f.fixture.registry.snapshot().unwrap().panes.len();
    let foreign = SpawnLayoutNode::Split {
        axis: Axis::Horizontal,
        ratio: 500,
        first: Box::new(f.layout()),
        second: Box::new(SpawnLayoutNode::Existing {
            pane_id: f.foreign.clone(),
        }),
    };
    assert!(
        f.raw(
            f.batch(1, foreign, "printf bad > forbidden-start"),
            AgentScope::Workspace
        )
        .is_err()
    );
    // Even session-wide authority does not make a foreign window's pane a valid
    // Existing node in this window. Native ownership remains authoritative.
    let foreign = SpawnLayoutNode::Split {
        axis: Axis::Horizontal,
        ratio: 500,
        first: Box::new(f.layout()),
        second: Box::new(SpawnLayoutNode::Existing {
            pane_id: f.foreign.clone(),
        }),
    };
    assert!(
        f.raw(
            f.batch(1, foreign, "printf bad > forbidden-start"),
            AgentScope::Session
        )
        .is_err()
    );
    f.call("window.rename", json!({"workspace_id":f.workspace,"window_id":f.window,"expected_revision":1,"name":"revision-two"}), AgentScope::Workspace).unwrap();
    let stale = f
        .raw(
            f.batch(1, f.layout(), "printf bad > forbidden-start"),
            AgentScope::Workspace,
        )
        .unwrap_err();
    assert!(stale.to_string().contains("stale"));
    assert!(!f.fixture.home.join("forbidden-start").exists());
    assert_eq!(f.fixture.registry.snapshot().unwrap().panes.len(), before);
    let mut operation = f.batch(
        2,
        f.layout(),
        "printf ready > batch-ready; while IFS= read -r line; do :; done",
    );
    if let Operation::PaneSpawn { panes, layout, .. } = &mut operation {
        panes.push(PaneSpawnEntry {
            reference: "second".into(),
            spec: control_spec(&f.fixture.home, "while IFS= read -r line; do :; done"),
        });
        *layout = Some(SpawnLayoutNode::Split {
            axis: Axis::Vertical,
            ratio: 500,
            first: Box::new(f.layout()),
            second: Box::new(SpawnLayoutNode::New {
                reference: "second".into(),
            }),
        });
    }
    let result = f.raw(operation, AgentScope::Workspace).unwrap();
    await_file(&f.fixture.home.join("batch-ready"));
    let created = result["created_panes"]["new"].as_str().unwrap();
    assert_eq!(result["registry"]["panes"].as_array().unwrap().len(), 3);
    let registry = f.fixture.registry.snapshot().unwrap();
    let window = registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == f.workspace)
        .unwrap()
        .windows
        .iter()
        .find(|window| window.id == f.window)
        .unwrap();
    assert_eq!(window.revision, 3);
    assert!(layout_references(window.layout.as_ref().unwrap(), &f.pane));
    assert!(layout_references(window.layout.as_ref().unwrap(), created));
    assert!(layout_references(
        window.layout.as_ref().unwrap(),
        result["created_panes"]["second"].as_str().unwrap()
    ));
}

#[test]
fn authenticated_control_read_input_use_private_views_without_changing_human_authority() {
    let f = ControlFixture::new();
    let human = human_view(&f);
    let before = f.fixture.snapshot(&f.pane);
    for operation in ["pane.read", "pane.inspect"] {
        let result = f
            .call(operation, json!({"pane_id":f.pane}), AgentScope::Workspace)
            .unwrap();
        assert!(result.get("views").is_none());
        assert!(result.get("view_id").is_none());
        assert!(result.get("frame").is_none());
        assert!(result.get("images").is_none());
        if operation == "pane.read" {
            assert!(result["text"].is_string());
        } else {
            assert!(result["text"].is_null());
        }
    }
    let sent = f
        .call(
            "input.text",
            json!({"pane_id":f.pane,"text":"alpha"}),
            AgentScope::Workspace,
        )
        .unwrap();
    assert_eq!(sent["accepted"], true);
    assert_eq!(sent["semantics"], "queued_once_no_replay");
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let result = f
            .call(
                "pane.read",
                json!({"pane_id":f.pane}),
                AgentScope::Workspace,
            )
            .unwrap();
        if result["text"].as_str().unwrap().contains("alpha") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "input did not reach native terminal"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !f.fixture.home.join("control-input").exists(),
        "input.text appended Enter"
    );
    f.call(
        "input.bytes",
        json!({"pane_id":f.pane,"data":[10]}),
        AgentScope::Workspace,
    )
    .unwrap();
    await_file(&f.fixture.home.join("control-input"));
    assert_eq!(
        fs::read(f.fixture.home.join("control-input")).unwrap(),
        b"alpha"
    );
    f.call(
        "input.paste",
        json!({"pane_id":f.pane,"text":"beta"}),
        AgentScope::Workspace,
    )
    .unwrap();
    f.call(
        "input.key",
        json!({"pane_id":f.pane,"event":{
            "key":"Enter","modifiers":0,"text":"","action":"Press","unshifted_codepoint":0
        }}),
        AgentScope::Workspace,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while fs::read(f.fixture.home.join("control-input")).unwrap() != b"beta" {
        assert!(
            std::time::Instant::now() < deadline,
            "paste/key input did not reach native terminal"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        f.raw(
            Operation::Pane {
                pane_id: f.pane.clone(),
                request: PaneRequest::Input {
                    view_id: human.clone(),
                    data: b"forbidden".to_vec()
                }
            },
            AgentScope::Workspace
        )
        .is_err()
    );
    assert!(
        f.raw(
            Operation::Pane {
                pane_id: f.pane.clone(),
                request: PaneRequest::Poll {
                    view_id: human,
                    after: 0,
                    frame_after: 0,
                    resources: Vec::new(),
                    snapshot_after: 0,
                    wait_ms: 0,
                    row_deltas: false,
                    scene_encoding: None
                }
            },
            AgentScope::Workspace
        )
        .is_err()
    );
    let after = f.fixture.snapshot(&f.pane);
    assert_eq!(before.views.len(), after.views.len());
    assert_eq!(before.views[0].id, after.views[0].id);
    assert_eq!(
        before
            .size_owner
            .as_ref()
            .map(|owner| (&owner.view_id, owner.size)),
        after
            .size_owner
            .as_ref()
            .map(|owner| (&owner.view_id, owner.size))
    );
    // Private control views remain transient even with many human views.
    for _ in 0..40 {
        human_view(&f);
    }
    let humans = f.fixture.snapshot(&f.pane);
    for operation in ["pane.read", "input.text"] {
        let arguments = if operation == "input.text" {
            json!({"pane_id":f.pane,"text":"many-views"})
        } else {
            json!({"pane_id":f.pane})
        };
        f.call(operation, arguments, AgentScope::Workspace).unwrap();
        let after = f.fixture.snapshot(&f.pane);
        assert_eq!(
            serde_json::to_value(&after.views).unwrap(),
            serde_json::to_value(&humans.views).unwrap()
        );
        assert_eq!(
            after
                .size_owner
                .as_ref()
                .map(|owner| (&owner.view_id, owner.size)),
            humans
                .size_owner
                .as_ref()
                .map(|owner| (&owner.view_id, owner.size))
        );
        assert_eq!(after.lifecycle, humans.lifecycle);
    }
}

#[test]
fn authenticated_control_revocation_during_reserved_spawn_prevents_exec_and_publication() {
    for terminate in [false, true] {
        let f = ControlFixture::new();
        let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
        *f.fixture.registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
            entered,
            resume: Mutex::new(resume_rx),
        }));
        let registry = f.fixture.registry.clone();
        let grant = f.grant.clone();
        let peer = Peer {
            pid: f.peer.pid,
            uid: f.peer.uid,
            birth: f.peer.birth.clone(),
        };
        let operation = f.batch(1, f.layout(), "printf bad > forbidden-start");
        let worker = thread::spawn(move || {
            control_request(&registry, &grant, &peer, operation, AgentScope::Workspace)
        });
        let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
        let ended = if terminate {
            f.fixture
                .registry
                .request(Request::new(Operation::PaneTerminate {
                    pane_id: f.pane.clone(),
                }))
        } else {
            f.fixture.registry.request_authenticated(
                Request::new(Operation::HarnessEvent {
                    launch_id: f.grant.launch_id.clone(),
                    token: f.grant.token.clone(),
                    event: CaptureEvent::End,
                }),
                &f.peer,
            )
        };
        let _ = resume.send(());
        let result = worker.join().unwrap();
        blocked.unwrap();
        ended.unwrap();
        assert!(result.is_err());
        assert!(!f.fixture.home.join("forbidden-start").exists());
        let state = f.fixture.registry.inner.state.lock().unwrap();
        let counts = (
            state.record.registry.panes.len(),
            state.spawning,
            state.spawn_workspaces.len(),
        );
        drop(state);
        assert_eq!(counts, (2, 0, 0));
    }
}

#[test]
fn authenticated_control_observation_does_not_hold_the_registry_writer() {
    let f = ControlFixture::new();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *f.fixture
        .registry
        .inner
        .control_observe_barrier
        .lock()
        .unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let registry = f.fixture.registry.clone();
    let grant = f.grant.clone();
    let peer = Peer {
        pid: f.peer.pid,
        uid: f.peer.uid,
        birth: f.peer.birth.clone(),
    };
    let worker = thread::spawn(move || {
        control_request(
            &registry,
            &grant,
            &peer,
            Operation::Inspect,
            AgentScope::Workspace,
        )
    });
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let writer_available = f.fixture.registry.inner.transaction.try_lock().is_ok();
    // End must finish while process observation is still paused, not after the
    // observer or its sampler wait releases the session-wide writer.
    let ended = if writer_available {
        f.fixture.registry.request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: f.grant.launch_id.clone(),
                token: f.grant.token.clone(),
                event: CaptureEvent::End,
            }),
            &f.peer,
        )
    } else {
        Err(anyhow::anyhow!(
            "process observation held the registry writer"
        ))
    };
    let _ = resume.send(());
    let result = worker.join().unwrap();
    blocked.unwrap();
    assert!(writer_available);
    ended.unwrap();
    assert!(result.is_err());
}

#[test]
fn authenticated_control_rechecks_before_each_child_and_before_publication() {
    for terminate in [false, true] {
        for child_count in [1, 2] {
            let f = ControlFixture::new();
            let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
            let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
            let resume_rx = Mutex::new(resume_rx);
            let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = starts.clone();
            *f.fixture.registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |_| {
                if observed.fetch_add(1, Ordering::AcqRel) == 0 {
                    let _ = entered.send(());
                    let _ = resume_rx.lock().unwrap().recv();
                }
            }));
            let first = PaneSpawnEntry {
                reference: "first".into(),
                spec: control_spec(
                    &f.fixture.home,
                    "printf ready > first-control-child; while IFS= read -r line; do :; done",
                ),
            };
            let mut panes = vec![first];
            let mut child_layout = SpawnLayoutNode::New {
                reference: "first".into(),
            };
            if child_count == 2 {
                panes.push(PaneSpawnEntry {
                    reference: "second".into(),
                    spec: control_spec(
                        &f.fixture.home,
                        "printf bad > forbidden-second-child; while IFS= read -r line; do :; done",
                    ),
                });
                child_layout = SpawnLayoutNode::Split {
                    axis: Axis::Vertical,
                    ratio: 500,
                    first: Box::new(child_layout),
                    second: Box::new(SpawnLayoutNode::New {
                        reference: "second".into(),
                    }),
                };
            }
            let operation = Operation::PaneSpawn {
                workspace_id: Some(f.workspace.clone()),
                window_id: Some(f.window.clone()),
                expected_revision: Some(1),
                panes,
                layout: Some(SpawnLayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 500,
                    first: Box::new(SpawnLayoutNode::Existing {
                        pane_id: f.pane.clone(),
                    }),
                    second: Box::new(child_layout),
                }),
            };
            let registry = f.fixture.registry.clone();
            let grant = f.grant.clone();
            let peer = Peer {
                pid: f.peer.pid,
                uid: f.peer.uid,
                birth: f.peer.birth.clone(),
            };
            let worker = thread::spawn(move || {
                control_request(&registry, &grant, &peer, operation, AgentScope::Workspace)
            });
            let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
            let first_marker = f.fixture.home.join("first-control-child");
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while blocked.is_ok() && !first_marker.exists() && std::time::Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(5));
            }
            let first_started = first_marker.exists();
            let ended = if terminate {
                f.fixture
                    .registry
                    .request(Request::new(Operation::PaneTerminate {
                        pane_id: f.pane.clone(),
                    }))
            } else {
                f.fixture.registry.request_authenticated(
                    Request::new(Operation::HarnessEvent {
                        launch_id: f.grant.launch_id.clone(),
                        token: f.grant.token.clone(),
                        event: CaptureEvent::End,
                    }),
                    &f.peer,
                )
            };
            let _ = resume.send(());
            let result = worker.join().unwrap();
            blocked.unwrap();
            ended.unwrap();
            assert!(
                first_started,
                "first child did not execute the boundary fixture"
            );
            assert!(result.is_err());
            assert_eq!(starts.load(Ordering::Acquire), 1);
            assert!(!f.fixture.home.join("forbidden-second-child").exists());
            let state = f.fixture.registry.inner.state.lock().unwrap();
            let counts = (
                state.record.registry.panes.len(),
                state.spawning,
                state.spawn_workspaces.len(),
            );
            drop(state);
            assert_eq!(counts, (2, 0, 0));
        }
    }
}

#[test]
fn managed_and_unmanaged_headless_requests_preserve_input_and_cleanup_receipts() {
    let f = ControlFixture::new();
    let (stream, _lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
    let session = &f.fixture.registry.inner.session;
    let (lease, registered) = f
        .fixture
        .registry
        .register_client(&stream, &session.session_id, &session.daemon_id)
        .unwrap();
    let authority: ClientAuthority = serde_json::from_value(registered["client"].clone()).unwrap();
    f.fixture
        .registry
        .client_request(
            authority.clone(),
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: None,
            },
            &stream,
        )
        .unwrap();
    for name in [
        "pane.read",
        "input.text",
        "input.paste",
        "input.bytes",
        "input.key",
    ] {
        let arguments = match name {
            "pane.read" => json!({"pane_id":f.pane}),
            "input.bytes" => json!({"pane_id":f.pane,"data":[120]}),
            "input.key" => json!({"pane_id":f.pane,"event":{
                "key":"Enter","modifiers":0,"text":"","action":"Press","unshifted_codepoint":0
            }}),
            _ => json!({"pane_id":f.pane,"text":"managed"}),
        };
        let ControlAction::Native(operation) = resolve(name, arguments).unwrap() else {
            panic!("test expected native operation");
        };
        let result = f
            .fixture
            .registry
            .client_request(authority.clone(), operation, &stream)
            .unwrap()
            .into_json()
            .unwrap();
        if name == "pane.read" {
            assert!(result["text"].is_string());
        } else {
            assert_eq!(result["accepted"], true);
            assert!(result["write_id"].is_u64());
        }
        assert!(f.fixture.snapshot(&f.pane).views.is_empty());
    }
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
    // Hold a successful receipt before cleanup to observe ownership, then retire
    // its client. Cleanup must be idempotent and must not erase the write ID.
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *f.fixture
        .registry
        .inner
        .control_cleanup_barrier
        .lock()
        .unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let registry = f.fixture.registry.clone();
    let worker_stream = stream.try_clone().unwrap();
    let worker_authority = authority.clone();
    let ControlAction::Native(operation) = resolve(
        "input.text",
        json!({"pane_id":f.pane,"text":"accepted-managed"}),
    )
    .unwrap() else {
        panic!("test expected native operation");
    };
    let worker =
        thread::spawn(move || registry.client_request(worker_authority, operation, &worker_stream));
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let pane = f.fixture.registry.inner.state.lock().unwrap().live[&f.pane].clone();
    let owners = pane
        .shared
        .authority
        .lock()
        .unwrap()
        .views
        .values()
        .map(|view| view.client.as_ref().map(|client| client.id.clone()))
        .collect::<Vec<_>>();
    f.fixture
        .registry
        .detach_client(&lease, "headless receipt test");
    let _ = resume.send(());
    let receipt = worker.join().unwrap();
    blocked.unwrap();
    assert_eq!(owners, vec![Some(lease.id.clone())]);
    let receipt = receipt.unwrap().into_json().unwrap();
    assert_eq!(receipt["accepted"], true);
    assert!(receipt["write_id"].is_u64());
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
    let ControlAction::Native(read) = resolve("pane.read", json!({"pane_id":f.pane})).unwrap()
    else {
        panic!("test expected native operation");
    };
    assert!(f.fixture.registry.request(Request::new(read)).is_ok());
    let ControlAction::Native(input) =
        resolve("input.text", json!({"pane_id":f.pane,"text":"raw-cli"})).unwrap()
    else {
        panic!("test expected native operation");
    };
    let sent = f
        .fixture
        .registry
        .request(Request::new(input))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(sent["accepted"], true);
    assert!(sent["write_id"].is_u64());
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
}

#[test]
fn accepted_control_input_keeps_its_receipt_when_temporary_view_expires_before_cleanup() {
    let f = ControlFixture::new();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *f.fixture
        .registry
        .inner
        .control_cleanup_barrier
        .lock()
        .unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let registry = f.fixture.registry.clone();
    let grant = f.grant.clone();
    let peer = Peer {
        pid: f.peer.pid,
        uid: f.peer.uid,
        birth: f.peer.birth.clone(),
    };
    let ControlAction::Native(operation) = resolve(
        "input.text",
        json!({"pane_id":f.pane,"text":"accepted-before-expiry"}),
    )
    .unwrap() else {
        panic!("test expected native operation");
    };
    let worker = thread::spawn(move || {
        control_request(&registry, &grant, &peer, operation, AgentScope::Workspace)
    });
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let pane = f.fixture.registry.inner.state.lock().unwrap().live[&f.pane].clone();
    let before = pane.shared.authority.lock().unwrap().views.len();
    let expiry = std::time::Instant::now() + Duration::from_secs(11);
    let expired = pane.shared.authority.lock().unwrap().expire(expiry);
    let _ = resume.send(());
    let result = worker.join().unwrap();
    blocked.unwrap();
    assert_eq!(before, 1);
    assert_eq!(expired.len(), 1);
    let result = result.unwrap();
    assert_eq!(result["accepted"], true);
    assert_eq!(result["semantics"], "queued_once_no_replay");
    assert!(result["write_id"].is_u64());
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
}

#[test]
fn queued_managed_headless_key_loses_its_client_grant_on_takeover() {
    let f = ControlFixture::new();
    let human = human_view(&f);
    let (stream, _lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
    let session = &f.fixture.registry.inner.session;
    let (lease, registered) = f
        .fixture
        .registry
        .register_client(&stream, &session.session_id, &session.daemon_id)
        .unwrap();
    let authority: ClientAuthority = serde_json::from_value(registered["client"].clone()).unwrap();
    f.fixture
        .registry
        .client_request(
            authority.clone(),
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: None,
            },
            &stream,
        )
        .unwrap();
    let pane = f.fixture.registry.inner.state.lock().unwrap().live[&f.pane].clone();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *pane.shared.resize_barrier.lock().unwrap() = Some(Arc::new(crate::ResizeBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let updated = f.fixture.registry.request(Request::new(Operation::Pane {
        pane_id: f.pane.clone(),
        request: PaneRequest::Update {
            view_id: human.clone(),
            spec: ViewSpec {
                size: Size {
                    cols: 120,
                    rows: 31,
                    cell_width: 8,
                    cell_height: 16,
                },
                visible: true,
                interactive: true,
                input: true,
                clipboard: true,
            },
        },
    }));
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let registry = f.fixture.registry.clone();
    let worker_stream = stream.try_clone().unwrap();
    let ControlAction::Native(operation) = resolve(
        "input.key",
        json!({"pane_id":f.pane,"event":{
            "key":"Enter","modifiers":0,"text":"","action":"Press","unshifted_codepoint":0
        }}),
    )
    .unwrap() else {
        panic!("test expected native operation");
    };
    let worker =
        thread::spawn(move || registry.client_request(authority, operation, &worker_stream));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while pane.commands.is_empty() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let queued = !pane.commands.is_empty();
    let temporary_owned = pane
        .shared
        .authority
        .lock()
        .unwrap()
        .views
        .values()
        .any(|view| {
            view.client
                .as_ref()
                .is_some_and(|client| client.id == lease.id)
        });
    let (new_stream, _new_lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_new_lease, registered) = f
        .fixture
        .registry
        .register_client(&new_stream, &session.session_id, &session.daemon_id)
        .unwrap();
    let new_authority: ClientAuthority =
        serde_json::from_value(registered["client"].clone()).unwrap();
    let takeover = f.fixture.registry.client_request(
        new_authority,
        Operation::ClientActivate {
            detach_others: true,
            agent_socket: None,
        },
        &new_stream,
    );
    let _ = resume.send(());
    let result = worker.join().unwrap();
    updated.unwrap();
    blocked.unwrap();
    takeover.unwrap();
    assert!(
        queued,
        "headless key never reached the blocked native queue"
    );
    assert!(
        temporary_owned,
        "temporary view lost its managed client owner"
    );
    assert!(!lease.is_active());
    assert!(result.is_err(), "revoked queued key was accepted");
    let remaining = f.fixture.snapshot(&f.pane).views;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, human);
    assert!(!f.fixture.home.join("control-input").exists());
}

#[test]
fn managed_headless_read_cannot_complete_after_client_detachment() {
    let f = ControlFixture::new();
    let (stream, _lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
    let session = &f.fixture.registry.inner.session;
    let (lease, registered) = f
        .fixture
        .registry
        .register_client(&stream, &session.session_id, &session.daemon_id)
        .unwrap();
    let authority: ClientAuthority = serde_json::from_value(registered["client"].clone()).unwrap();
    f.fixture
        .registry
        .client_request(
            authority.clone(),
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: None,
            },
            &stream,
        )
        .unwrap();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *f.fixture
        .registry
        .inner
        .control_cleanup_barrier
        .lock()
        .unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let registry = f.fixture.registry.clone();
    let worker_stream = stream.try_clone().unwrap();
    let ControlAction::Native(operation) = resolve("pane.read", json!({"pane_id":f.pane})).unwrap()
    else {
        panic!("test expected native operation");
    };
    let worker =
        thread::spawn(move || registry.client_request(authority, operation, &worker_stream));
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    f.fixture
        .registry
        .detach_client(&lease, "headless read test");
    let _ = resume.send(());
    let result = worker.join().unwrap();
    blocked.unwrap();
    assert!(
        result.is_err(),
        "read completed with a retired client grant"
    );
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
}

#[test]
fn authenticated_end_revokes_control_before_automatic_intent_durability_finishes() {
    let f = ControlFixture::new();
    bind_native(
        &f.fixture.registry,
        &f.grant,
        &f.peer,
        "control-end-durability",
    )
    .unwrap();
    let launch =
        f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
    let lease = launch.control_lease().unwrap();
    let pane = f.fixture.registry.inner.state.lock().unwrap().live[&f.pane].clone();
    let automatic_before = f
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .automatic
        .contains_key(&f.pane);
    assert!(
        automatic_before,
        "fixture did not establish durable automatic intent"
    );
    assert!(lease.is_valid());
    let attached = pane
        .request_control(
            PaneRequest::Attach {
                spec: ViewSpec {
                    size: Size {
                        cols: 80,
                        rows: 24,
                        cell_width: 8,
                        cell_height: 16,
                    },
                    visible: true,
                    interactive: false,
                    input: true,
                    clipboard: false,
                },
            },
            &lease,
        )
        .unwrap()
        .into_json()
        .unwrap();
    let view_id = attached["view_id"].as_str().unwrap().to_owned();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *f.fixture.registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let registry = f.fixture.registry.clone();
    let grant = f.grant.clone();
    let peer = Peer {
        pid: f.peer.pid,
        uid: f.peer.uid,
        birth: f.peer.birth.clone(),
    };
    let ending = thread::spawn(move || {
        registry.request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: grant.launch_id,
                token: grant.token,
                event: CaptureEvent::End,
            }),
            &peer,
        )
    });
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    // End has reached the existing commit barrier, but its automatic-intent
    // removal has not published and Captures::event(End) has not run yet.
    let automatic_during = f
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .automatic
        .contains_key(&f.pane);
    let valid_during = lease.is_valid();
    let rejected = pane.request_control(
        PaneRequest::Input {
            view_id: view_id.clone(),
            data: b"forbidden-during-end-durability\n".to_vec(),
        },
        &lease,
    );
    // Owned cleanup is independent of the revoked source launch and of the
    // writer waiting for durable End. Always release/join before asserting.
    let detached = pane.request(PaneRequest::Detach { view_id });
    let _ = resume.send(());
    let ended = ending.join().unwrap();
    blocked.unwrap();
    ended.unwrap();
    detached.unwrap();
    assert!(
        automatic_during,
        "End was not paused before durable publication"
    );
    assert!(
        !valid_during,
        "End left control authority live while waiting for durability"
    );
    assert!(
        rejected.is_err(),
        "input was admitted while End awaited durability"
    );
    assert!(!f.fixture.home.join("control-input").exists());
    assert!(f.fixture.snapshot(&f.pane).views.is_empty());
    let automatic_after = f
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .automatic
        .contains_key(&f.pane);
    assert!(!automatic_after);
}
