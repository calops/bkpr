use super::*;

fn title_spec(home: &Path, title: Option<&str>) -> PaneSpawnSpec {
    let mut launch = spec(home);
    launch.title = title.map(str::to_owned);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"printf '\033]0;first\007'; while IFS= read -r title; do printf '\033]2;%s\033\\' "$title"; i=0; while [ "$i" -lt 180 ]; do printf '\007'; i=$((i+1)); done; done"#.into(),
    ];
    launch
}
fn start(fixture: &Fixture, title: Option<&str>) -> String {
    send(
        &fixture.registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(title_spec(&fixture.home("home"), title)),
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn wait_title(registry: &Registry, id: &str, expected: &str) -> RegistrySnapshot {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let current = registry.snapshot().unwrap();
        if current
            .panes
            .iter()
            .any(|pane| pane.id == id && pane.title == expected)
        {
            let subscribed: RegistrySnapshot = serde_json::from_value(
                send(
                    registry,
                    Operation::Wait {
                        after: current.revision,
                        wait_ms: 0,
                    },
                )["registry"]
                    .clone(),
            )
            .unwrap();
            assert_eq!(
                subscribed
                    .panes
                    .iter()
                    .find(|pane| pane.id == id)
                    .unwrap()
                    .title,
                expected,
                "registry subscribers must receive the effective terminal title"
            );
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "title {expected:?} not published: {current:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_native(registry: &Registry, id: &str, expected: &str) {
    let pane = registry.capture_pane(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let actual = pane.shared.published.lock().unwrap().title.clone();
        if actual.as_deref().unwrap_or("") == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native title {expected:?} not received: {actual:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn input_title(registry: &Registry, id: &str, view: &str, title: &str) {
    send(
        registry,
        Operation::Pane {
            pane_id: id.into(),
            request: PaneRequest::Input {
                view_id: view.into(),
                data: format!("{title}\n").into_bytes(),
            },
        },
    );
    wait_native(registry, id, title);
}

#[test]
fn native_titles_update_hidden_registry_survive_event_eviction_and_clear_to_base() {
    let fixture = Fixture::new();
    let id = start(&fixture, None);
    let first = wait_title(&fixture.registry, &id, "first");
    assert!(
        first
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .unwrap()
            .harness_status
            .is_none()
    );
    assert!(
        fixture
            .registry
            .capture_pane(&id)
            .unwrap()
            .shared
            .authority
            .lock()
            .unwrap()
            .views
            .is_empty()
    );
    let live_view = view(&fixture.registry, &id, 80);
    input_title(&fixture.registry, &id, &live_view, "日本語 editor");
    let changed = wait_title(&fixture.registry, &id, "日本語 editor");
    assert!(changed.revision > first.revision);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let pane_snapshot: PaneSnapshot =
            serde_json::from_value(snapshot(&fixture.registry, &id)).unwrap();
        if pane_snapshot.events.len() == 128
            && !pane_snapshot
                .events
                .iter()
                .any(|event| matches!(event.effect, Effect::Title(_)))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native effect flood did not evict title event"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    send(
        &fixture.registry,
        Operation::Pane {
            pane_id: id.clone(),
            request: PaneRequest::Detach {
                view_id: live_view.clone(),
            },
        },
    );
    assert_eq!(
        wait_title(&fixture.registry, &id, "日本語 editor").panes[0].title,
        "日本語 editor"
    );
    let returned_view = view(&fixture.registry, &id, 80);
    input_title(&fixture.registry, &id, &returned_view, "");
    wait_title(&fixture.registry, &id, "sh");
    let saved: Value =
        serde_json::from_slice(&fs::read(fixture._root.path().join("registry.json")).unwrap())
            .unwrap();
    assert_eq!(saved["registry"]["panes"][0]["title"], "sh");
    assert!(
        saved["automatic_titles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == &id)
    );
}

#[test]
fn explicit_creation_and_rename_pin_labels_in_durable_metadata() {
    let fixture = Fixture::new();
    let named = start(&fixture, Some("build logs"));
    wait_native(&fixture.registry, &named, "first");
    assert_eq!(
        fixture.registry.snapshot().unwrap().panes[0].title,
        "build logs"
    );
    let automatic = start(&fixture, None);
    wait_title(&fixture.registry, &automatic, "first");
    send(
        &fixture.registry,
        Operation::PaneRename {
            pane_id: automatic.clone(),
            title: "my editor".into(),
        },
    );
    let live_view = view(&fixture.registry, &automatic, 80);
    input_title(
        &fixture.registry,
        &automatic,
        &live_view,
        "program replacement",
    );
    wait_title(&fixture.registry, &automatic, "my editor");
    let saved: Value =
        serde_json::from_slice(&fs::read(fixture._root.path().join("registry.json")).unwrap())
            .unwrap();
    let names = saved["registry"]["panes"].as_array().unwrap();
    assert_eq!(
        names.iter().find(|p| p["id"] == automatic).unwrap()["title"],
        "my editor"
    );
    assert_eq!(
        names.iter().find(|p| p["id"] == named).unwrap()["title"],
        "build logs"
    );
    assert!(saved["automatic_titles"].as_array().unwrap().is_empty());
}

fn reopen_copied_record(
    fixture: &Fixture,
    id: &str,
    legacy: bool,
) -> (Registry, tempfile::TempDir, String) {
    let root = private_tempdir();
    let mut saved: Value =
        serde_json::from_slice(&fs::read(fixture._root.path().join("registry.json")).unwrap())
            .unwrap();
    if legacy {
        saved.as_object_mut().unwrap().remove("automatic_titles");
    }
    fs::write(
        root.path().join("registry.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        root.path().join("registry.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let current = registry.snapshot().unwrap();
    assert_eq!(
        current
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .unwrap()
            .title,
        "sh"
    );
    let old = current.panes.iter().find(|pane| pane.id == id).unwrap();
    let workspace = current
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == old.workspace_id.as_ref())
        .unwrap();
    let window = workspace
        .windows
        .iter()
        .find(|w| Some(&w.id) == old.window_id.as_ref())
        .unwrap();
    let value = send(
        &registry,
        Operation::PaneReopen {
            workspace_id: workspace.id.clone(),
            window_id: window.id.clone(),
            expected_revision: window.revision,
            pane_id: id.into(),
            spec: title_spec(&root.path().join("replacement"), None),
        },
    );
    let new_id = value["pane_id"].as_str().unwrap().to_owned();
    (registry, root, new_id)
}

#[test]
fn automatic_intent_survives_storage_but_execution_titles_do_not() {
    let fixture = Fixture::new();
    let id = start(&fixture, None);
    wait_title(&fixture.registry, &id, "first");
    let (registry, _root, replacement) = reopen_copied_record(&fixture, &id, false);
    wait_title(&registry, &replacement, "first");
    registry.shutdown().unwrap();
}

// LEGACY_COMPAT(next-major): pre-title-policy records cannot distinguish an
// explicit rename from a generated label; preserve all such saved labels.
#[test]
fn legacy_compat_next_major_saved_labels_remain_fixed_without_origin_metadata() {
    let fixture = Fixture::new();
    let id = start(&fixture, None);
    wait_title(&fixture.registry, &id, "first");
    let (registry, _root, replacement) = reopen_copied_record(&fixture, &id, true);
    wait_native(&registry, &replacement, "first");
    wait_title(&registry, &replacement, "sh");
    registry.shutdown().unwrap();
}
