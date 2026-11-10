use crate::{
    controller::{leaves, split_leaf},
    transport::{self, Endpoint},
};
use anyhow::{Context, Result, bail};
use bkpr_protocol::*;

#[derive(Debug)]
pub(crate) enum Command {
    Single(Operation),
    Spawn {
        workspace: Workspace,
        window: Window,
        target: String,
        axis: Axis,
        spec: PaneSpawnSpec,
    },
    Reopen {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        pane_id: String,
        spec: PaneSpawnSpec,
    },
    Create {
        name: Option<String>,
        spec: PaneSpawnSpec,
    },
    CreateWindow {
        workspace_id: String,
        expected_revision: u64,
        name: Option<String>,
        spec: PaneSpawnSpec,
    },
}

#[derive(Debug)]
pub(crate) struct Selection {
    pub workspace: String,
    pub window: String,
    pub pane: String,
}

#[derive(Debug)]
pub(crate) struct Completion {
    pub registry: RegistrySnapshot,
    pub selection: Option<Selection>,
}

// Management I/O stays on a worker. Ambiguous mutations are never replayed.
pub(crate) fn execute(socket: &Endpoint, command: Command) -> Result<Completion> {
    match command {
        Command::Single(operation) => single(socket, operation),
        Command::Spawn {
            workspace,
            window,
            target,
            axis,
            spec,
        } => spawn(socket, workspace, window, &target, axis, spec),
        Command::Reopen {
            workspace_id,
            window_id,
            expected_revision,
            pane_id,
            spec,
        } => {
            let replaced = request(socket, Operation::PaneReopen {
                workspace_id: workspace_id.clone(),
                window_id: window_id.clone(),
                expected_revision,
                pane_id: pane_id.clone(),
                spec,
            }, "new shell replacement")
                .context("new shell replacement was not confirmed; no retry was attempted. If the retained directory is unavailable, use Ctrl-b R and explicitly choose the client current directory")?;
            validate_result_target(&replaced, &workspace_id, &window_id)
                .map_err(transport::mark_outcome_unknown)?;
            let pane = replaced
                .pane_id
                .context("new shell replacement response omitted pane_id")
                .map_err(transport::mark_outcome_unknown)?;
            let window = owned_window(&replaced.registry, &workspace_id, &window_id)
                .map_err(transport::mark_outcome_unknown)?;
            let mut projected = Vec::new();
            leaves(window.layout.as_ref(), &mut projected);
            if pane == pane_id
                || projected.contains(&pane_id)
                || expected_revision.checked_add(1) != Some(window.revision)
            {
                bail_unknown(
                    "new shell replacement returned inconsistent identity, layout, or revision",
                )?;
            }
            validate_selection(&replaced.registry, &workspace_id, &window_id, &pane)
                .map_err(transport::mark_outcome_unknown)?;
            Ok(Completion {
                registry: replaced.registry,
                selection: Some(Selection {
                    workspace: workspace_id,
                    window: window_id,
                    pane,
                }),
            })
        }
        Command::Create { name, spec } => {
            let created = request(
                socket,
                Operation::WorkspaceCreate {
                    name,
                    initial: Some(spec),
                },
                "workspace creation",
            )?;
            creation(created, None)
        }
        Command::CreateWindow {
            workspace_id,
            expected_revision,
            name,
            spec,
        } => {
            let created = request(
                socket,
                Operation::WindowCreate {
                    workspace_id: workspace_id.clone(),
                    expected_revision,
                    name,
                    spec,
                },
                "window creation",
            )?;
            creation(created, Some((&workspace_id, expected_revision)))
        }
    }
}

fn bail_unknown(message: &str) -> Result<()> {
    Err(transport::mark_outcome_unknown(anyhow::anyhow!(
        "{message}; do not replay"
    )))
}

fn owned_window<'a>(
    registry: &'a RegistrySnapshot,
    workspace: &str,
    window: &str,
) -> Result<&'a Window> {
    registry
        .workspaces
        .iter()
        .find(|entry| entry.id == workspace)
        .context("response omitted target workspace")?
        .windows
        .iter()
        .find(|entry| entry.id == window)
        .context("response omitted target window")
}

fn validate_result_target(result: &RegistryResult, workspace: &str, window: &str) -> Result<()> {
    if result
        .workspace_id
        .as_deref()
        .is_some_and(|id| id != workspace)
        || result.window_id.as_deref().is_some_and(|id| id != window)
    {
        bail!("response returned a different target workspace or window identity");
    }
    Ok(())
}

fn validate_selection(
    registry: &RegistrySnapshot,
    workspace: &str,
    window: &str,
    pane: &str,
) -> Result<()> {
    let document = owned_window(registry, workspace, window)?;
    let entry = registry
        .panes
        .iter()
        .find(|entry| entry.id == pane)
        .context("response omitted created pane")?;
    let mut projected = Vec::new();
    leaves(document.layout.as_ref(), &mut projected);
    if entry.workspace_id.as_deref() != Some(workspace)
        || entry.window_id.as_deref() != Some(window)
        || entry.status != PaneStatus::Running
        || !projected.iter().any(|id| id == pane)
    {
        bail!("response returned inconsistent pane ownership, status, or window projection");
    }
    Ok(())
}

fn creation(created: RegistryResult, target: Option<(&str, u64)>) -> Result<Completion> {
    (|| {
        let workspace = created
            .workspace_id
            .context("creation response omitted workspace_id")?;
        let window = created
            .window_id
            .context("creation response omitted window_id")?;
        let pane = created
            .pane_id
            .context("creation response omitted pane_id")?;
        if let Some((expected, revision)) = target
            && (workspace != expected
                || created
                    .registry
                    .workspaces
                    .iter()
                    .find(|entry| entry.id == workspace)
                    .is_none_or(|entry| revision.checked_add(1) != Some(entry.revision)))
        {
            bail!("window creation response returned inconsistent workspace identity or revision");
        }
        validate_selection(&created.registry, &workspace, &window, &pane)?;
        Ok(Completion {
            registry: created.registry,
            selection: Some(Selection {
                workspace,
                window,
                pane,
            }),
        })
    })()
    .map_err(transport::mark_outcome_unknown)
    .context(
        "atomic creation succeeded but its returned identities were not confirmed; do not replay",
    )
}

fn spawn(
    socket: &Endpoint,
    workspace: Workspace,
    window: Window,
    target: &str,
    axis: Axis,
    spec: PaneSpawnSpec,
) -> Result<Completion> {
    if !workspace
        .windows
        .iter()
        .any(|entry| entry.id == window.id && entry.revision == window.revision)
    {
        bail!("captured window no longer belongs to the captured workspace");
    }
    let mut projected = Vec::new();
    leaves(window.layout.as_ref(), &mut projected);
    if window.layout.is_some() && !projected.iter().any(|pane| pane == target) {
        bail!("split target is not projected in the captured window");
    }
    const REFERENCE: &str = "split";
    let created = request(
        socket,
        Operation::PaneSpawn {
            workspace_id: Some(workspace.id.clone()),
            window_id: Some(window.id.clone()),
            expected_revision: Some(window.revision),
            panes: vec![PaneSpawnEntry {
                reference: REFERENCE.into(),
                spec,
            }],
            layout: Some(spawn_layout(
                window.layout.as_ref(),
                target,
                axis,
                REFERENCE,
            )),
        },
        "pane creation",
    )?;
    (|| {
        validate_result_target(&created, &workspace.id, &window.id)?;
        let pane = created
            .created_panes
            .get(REFERENCE)
            .context("pane creation response omitted its request-local reference")?
            .clone();
        if created.created_panes.len() != 1
            || created.pane_id.as_ref().is_some_and(|id| id != &pane)
            || projected.contains(&pane)
        {
            bail!("pane creation returned inconsistent new pane identities");
        }
        validate_selection(&created.registry, &workspace.id, &window.id, &pane)?;
        let document = owned_window(&created.registry, &workspace.id, &window.id)?;
        let expected = split_leaf(window.layout, target, &pane, axis);
        if document.layout != expected || window.revision.checked_add(1) != Some(document.revision)
        {
            bail!("pane creation did not confirm its exact final layout and window revision");
        }
        Ok(Completion {
            registry: created.registry,
            selection: Some(Selection {
                workspace: workspace.id,
                window: window.id,
                pane,
            }),
        })
    })()
    .map_err(transport::mark_outcome_unknown)
    .context("pane creation succeeded but its final placement was not confirmed; do not replay")
}

fn spawn_layout(
    layout: Option<&LayoutNode>,
    target: &str,
    axis: Axis,
    reference: &str,
) -> SpawnLayoutNode {
    let new = || SpawnLayoutNode::New {
        reference: reference.into(),
    };
    match layout {
        None => new(),
        Some(LayoutNode::Leaf { pane_id }) => {
            let existing = SpawnLayoutNode::Existing {
                pane_id: pane_id.clone(),
            };
            if pane_id == target {
                SpawnLayoutNode::Split {
                    axis,
                    ratio: 500,
                    first: Box::new(existing),
                    second: Box::new(new()),
                }
            } else {
                existing
            }
        }
        Some(LayoutNode::Split {
            axis: existing_axis,
            ratio,
            first,
            second,
        }) => SpawnLayoutNode::Split {
            axis: *existing_axis,
            ratio: *ratio,
            first: Box::new(spawn_layout(Some(first), target, axis, reference)),
            second: Box::new(spawn_layout(Some(second), target, axis, reference)),
        },
    }
}

fn single(socket: &Endpoint, operation: Operation) -> Result<Completion> {
    let expected = operation.clone();
    let result = request(socket, operation, "operation")?;
    let registry = &result.registry;
    let confirmation = (|| -> Result<()> {
        match expected {
            Operation::LayoutUpdate {
                workspace_id,
                window_id,
                expected_revision,
                layout,
            } => {
                let window = owned_window(registry, &workspace_id, &window_id)?;
                if expected_revision.checked_add(1) != Some(window.revision)
                    || window.layout != layout
                {
                    bail!("layout response did not confirm exact window revision and projection");
                }
                let mut projected = Vec::new();
                leaves(window.layout.as_ref(), &mut projected);
                for pane in projected {
                    let pane = registry
                        .panes
                        .iter()
                        .find(|entry| entry.id == pane)
                        .context("layout response omitted a projected pane")?;
                    if pane.workspace_id.as_ref() != Some(&workspace_id)
                        || pane.window_id.as_ref() != Some(&window_id)
                    {
                        bail!("layout response projected a foreign terminal");
                    }
                }
            }
            Operation::WindowRename {
                workspace_id,
                window_id,
                expected_revision,
                name,
            } => {
                let window = owned_window(registry, &workspace_id, &window_id)?;
                if expected_revision.checked_add(1) != Some(window.revision) || window.name != name
                {
                    bail!(
                        "rename response did not confirm exact window identity, revision, and name"
                    );
                }
            }
            Operation::WorkspaceRename {
                workspace_id,
                expected_revision,
                name,
            } => {
                let workspace = registry
                    .workspaces
                    .iter()
                    .find(|entry| entry.id == workspace_id)
                    .context("rename response omitted target workspace")?;
                if expected_revision.checked_add(1) != Some(workspace.revision)
                    || workspace.name != name
                {
                    bail!(
                        "rename response did not confirm exact workspace identity, revision, and name"
                    );
                }
            }
            Operation::WindowClose {
                workspace_id,
                window_id,
                ..
            } => {
                if registry.workspaces.iter().any(|workspace| {
                    workspace.id == workspace_id
                        && workspace
                            .windows
                            .iter()
                            .any(|window| window.id == window_id)
                }) || registry
                    .panes
                    .iter()
                    .any(|pane| pane.window_id.as_ref() == Some(&window_id))
                {
                    bail!("close response retained the target window or its terminal membership");
                }
            }
            Operation::WorkspaceClose { workspace_id, .. } => {
                if registry
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == workspace_id)
                    || registry
                        .panes
                        .iter()
                        .any(|pane| pane.workspace_id.as_ref() == Some(&workspace_id))
                {
                    bail!(
                        "close response retained the target workspace or its terminal membership"
                    );
                }
            }
            Operation::PaneRename { pane_id, title } => {
                if !registry
                    .panes
                    .iter()
                    .any(|pane| pane.id == pane_id && pane.title == title)
                {
                    bail!("rename response did not confirm the exact terminal identity and title");
                }
            }
            Operation::PaneTerminate { pane_id } => {
                if registry.panes.iter().any(|pane| {
                    pane.id == pane_id
                        && (pane.status == PaneStatus::Running
                            || pane.workspace_id.is_some()
                            || pane.window_id.is_some())
                }) {
                    bail!("close response retained a running or owned target terminal");
                }
            }
            _ => bail!("unsupported interactive management confirmation"),
        }
        Ok(())
    })();
    confirmation.map_err(transport::mark_outcome_unknown)
        .context("operation succeeded but its returned registry did not confirm the mutation; do not replay")?;
    Ok(Completion {
        registry: result.registry,
        selection: None,
    })
}

fn request(socket: &Endpoint, operation: Operation, phase: &str) -> Result<RegistryResult> {
    let value = socket.checked(operation)?;
    serde_json::from_value(value)
        .map_err(|error| transport::mark_outcome_unknown(error.into()))
        .with_context(|| {
            format!("{phase} already succeeded, but the response omitted a valid registry result; do not replay")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    fn leaf(pane: &str) -> LayoutNode {
        LayoutNode::Leaf {
            pane_id: pane.into(),
        }
    }

    fn split(axis: Axis, ratio: u16, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn workspace(revision: u64, layout: Option<LayoutNode>) -> Workspace {
        Workspace {
            id: "workspace".into(),
            name: "work".into(),
            revision: 100,
            windows: vec![Window {
                id: "window".into(),
                name: "terminal".into(),
                revision,
                layout,
            }],
        }
    }

    fn registry(revision: u64, workspace: Workspace, panes: &[&str]) -> RegistrySnapshot {
        RegistrySnapshot {
            revision,
            workspaces: vec![workspace],
            panes: panes
                .iter()
                .map(|id| PaneInfo {
                    id: (*id).into(),
                    workspace_id: Some("workspace".into()),
                    window_id: Some("window".into()),
                    title: (*id).into(),
                    status: PaneStatus::Running,
                    exit_code: None,
                    cwd: "/tmp".into(),
                    foreground_cwd: None,
                    harness: None,
                    harness_status: None,
                    project: None,
                    home: "/tmp".into(),
                    isolated_environment: false,
                    harness_status_changed_at_ms: None,
                })
                .collect(),
        }
    }

    fn spec() -> PaneSpawnSpec {
        PaneSpawnSpec {
            argv: vec!["sh".into()],
            cwd: "/tmp".into(),
            home: "/tmp".into(),
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            title: Some("new pane".into()),
            isolated_environment: false,
        }
    }

    fn success(
        registry: RegistrySnapshot,
        pane: Option<&str>,
        workspace: Option<&str>,
    ) -> Option<Response> {
        Some(Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(
                serde_json::to_value(RegistryResult {
                    registry,
                    pane_id: pane.map(str::to_owned),
                    workspace_id: workspace.map(str::to_owned),
                    window_id: pane.map(|_| "window".to_owned()),
                    created_panes: pane
                        .map(|id| ("split".to_owned(), id.to_owned()))
                        .into_iter()
                        .collect(),
                })
                .unwrap()
                .into(),
            ),
            error: None,
            failure: None,
        })
    }

    fn rejection(message: &str) -> Option<Response> {
        Some(Response {
            version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some(message.into()),
            failure: None,
        })
    }

    // Keep accepting until execute returns, so an accidental replay is recorded
    // and rejected rather than disappearing behind a closed fixture listener.
    fn scripted(
        replies: Vec<Option<Response>>,
        command: Command,
    ) -> (Result<Completion>, Vec<Request>) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("management.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (done, finished) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut replies = replies.into_iter();
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let request = transport::test_wire::read_request(&mut stream).unwrap();
                        assert_eq!(request.version, PROTOCOL_VERSION);
                        requests.push(request);
                        if let Some(reply) = replies
                            .next()
                            .unwrap_or_else(|| rejection("unexpected mutation replay"))
                        {
                            transport::test_wire::write_response(&mut stream, &reply).unwrap();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if finished.try_recv().is_ok() {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "management worker did not finish"
                        );
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accepting management request: {error}"),
                }
            }
            assert!(
                replies.next().is_none(),
                "expected management request was omitted"
            );
            requests
        });
        let result = execute(&Endpoint::unmanaged(&socket), command);
        done.send(()).unwrap();
        (result, server.join().unwrap())
    }

    fn spawn_command(workspace: Workspace) -> Command {
        Command::Spawn {
            window: workspace.windows[0].clone(),
            workspace,
            target: "old".into(),
            axis: Axis::Vertical,
            spec: spec(),
        }
    }
    fn reopen_command() -> Command {
        Command::Reopen {
            workspace_id: "workspace".into(),
            window_id: "window".into(),
            expected_revision: 7,
            pane_id: "old".into(),
            spec: spec(),
        }
    }

    #[test]
    fn reopen_is_one_atomic_request_with_new_selection_and_explicit_spawn_spec() {
        let mut snapshot = registry(20, workspace(8, Some(leaf("new"))), &["old", "new"]);
        snapshot.panes[0].status = PaneStatus::Stopped;
        let (result, requests) =
            scripted(vec![success(snapshot, Some("new"), None)], reopen_command());
        let completed = result.unwrap();
        let selection = completed.selection.unwrap();
        assert_eq!(selection.workspace, "workspace");
        assert_eq!(selection.window, "window");
        assert_eq!(selection.pane, "new");
        assert_eq!(completed.registry.panes[0].status, PaneStatus::Stopped);
        assert_eq!(requests.len(), 1);
        match &requests[0].operation {
            Operation::PaneReopen {
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
                spec: requested,
            } => {
                assert_eq!(workspace_id, "workspace");
                assert_eq!(window_id, "window");
                assert_eq!(*expected_revision, 7);
                assert_eq!(pane_id, "old");
                assert_eq!(requested.argv, spec().argv);
                assert_eq!(requested.cwd, spec().cwd);
                assert_eq!(requested.home, spec().home);
                assert_eq!(requested.isolated_environment, spec().isolated_environment);
            }
            other => panic!("unexpected replacement request: {other:?}"),
        }
    }

    #[test]
    fn rejected_and_ambiguous_reopen_never_send_layout_changes_or_retry() {
        for (reply, unknown) in [
            (rejection("revision changed"), false),
            (rejection("directory missing"), false),
            (None, true),
        ] {
            let (result, requests) = scripted(vec![reply], reopen_command());
            let error = result.unwrap_err();
            assert_eq!(transport::outcome_unknown(&error), unknown);
            assert_eq!(requests.len(), 1);
            assert!(matches!(
                requests[0].operation,
                Operation::PaneReopen { .. }
            ));
        }
    }

    #[test]
    fn spawn_uses_captured_layout_and_exact_window_revisions() {
        let old_layout = split(Axis::Horizontal, 325, leaf("old"), leaf("other"));
        let expected_layout = split(
            Axis::Horizontal,
            325,
            split(Axis::Vertical, 500, leaf("old"), leaf("new")),
            leaf("other"),
        );
        // Global/workspace revisions are independent from the captured window CAS.
        let (result, requests) = scripted(
            vec![success(
                registry(
                    900,
                    workspace(8, Some(expected_layout)),
                    &["old", "other", "new"],
                ),
                Some("new"),
                None,
            )],
            spawn_command(workspace(7, Some(old_layout))),
        );
        let completion = result.unwrap();
        assert_eq!(requests.len(), 1);
        match &requests[0].operation {
            Operation::PaneSpawn {
                workspace_id,
                window_id,
                expected_revision,
                panes,
                layout,
            } => {
                assert_eq!(workspace_id.as_deref(), Some("workspace"));
                assert_eq!(window_id.as_deref(), Some("window"));
                assert_eq!(*expected_revision, Some(7));
                assert_eq!(panes[0].reference, "split");
                assert_eq!(
                    serde_json::to_value(layout).unwrap(),
                    serde_json::json!({
                        "kind": "split", "axis": "horizontal", "ratio": 325,
                        "first": {
                            "kind": "split", "axis": "vertical", "ratio": 500,
                            "first": {"kind": "existing", "pane_id": "old"},
                            "second": {"kind": "new", "reference": "split"}
                        },
                        "second": {"kind": "existing", "pane_id": "other"}
                    })
                );
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        assert_eq!(completion.registry.revision, 900);
        let selected = completion.selection.unwrap();
        assert_eq!(selected.workspace, "workspace");
        assert_eq!(selected.window, "window");
        assert_eq!(selected.pane, "new");
    }

    #[test]
    fn rejected_or_lost_atomic_spawn_is_never_replayed_or_followed_by_placement() {
        for (reply, unknown) in [(rejection("revision conflict"), false), (None, true)] {
            let (result, requests) =
                scripted(vec![reply], spawn_command(workspace(7, Some(leaf("old")))));
            assert_eq!(transport::outcome_unknown(&result.unwrap_err()), unknown);
            assert_eq!(requests.len(), 1);
            assert!(matches!(requests[0].operation, Operation::PaneSpawn { .. }));
        }
    }

    #[test]
    fn successful_spawn_ack_must_confirm_mapping_and_final_layout_before_selecting() {
        for fault in ["mapping", "layout", "revision"] {
            let mut reply = success(
                registry(
                    900,
                    workspace(
                        8,
                        Some(split(Axis::Vertical, 500, leaf("old"), leaf("new"))),
                    ),
                    &["old", "new"],
                ),
                Some("new"),
                None,
            )
            .unwrap();
            let Some(Payload::Control(value)) = &mut reply.result else {
                panic!("fixture response is not a control result");
            };
            match fault {
                "mapping" => value["created_panes"] = serde_json::json!({"other": "new"}),
                "layout" => {
                    value["registry"]["workspaces"][0]["windows"][0]["layout"] =
                        serde_json::to_value(leaf("old")).unwrap()
                }
                "revision" => {
                    value["registry"]["workspaces"][0]["windows"][0]["revision"] = 9.into()
                }
                _ => unreachable!(),
            }
            let (result, requests) = scripted(
                vec![Some(reply)],
                spawn_command(workspace(7, Some(leaf("old")))),
            );
            assert!(transport::outcome_unknown(&result.unwrap_err()), "{fault}");
            assert_eq!(requests.len(), 1, "{fault} must not replay the mutation");
        }
    }

    #[test]
    fn ordinary_workspace_creation_is_atomic_and_delegates_blank_naming() {
        let (result, requests) = scripted(
            vec![success(
                registry(10, workspace(1, Some(leaf("new"))), &["new"]),
                Some("new"),
                Some("workspace"),
            )],
            Command::Create {
                name: None,
                spec: spec(),
            },
        );
        let selection = result.unwrap().selection.unwrap();
        assert_eq!(
            (
                selection.workspace.as_str(),
                selection.window.as_str(),
                selection.pane.as_str()
            ),
            ("workspace", "window", "new")
        );
        assert_eq!(requests.len(), 1);
        match &requests[0].operation {
            Operation::WorkspaceCreate {
                name,
                initial: Some(initial),
            } => {
                assert!(name.is_none());
                assert_eq!(initial.cwd, spec().cwd);
                assert_eq!(initial.argv, spec().argv);
            }
            other => panic!("unexpected atomic creation request: {other:?}"),
        }
    }

    #[test]
    fn create_window_is_separate_and_uses_the_captured_workspace_revision() {
        let mut snapshot = registry(20, workspace(1, Some(leaf("new"))), &["new"]);
        snapshot.workspaces[0].revision = 101;
        let (result, requests) = scripted(
            vec![success(snapshot, Some("new"), Some("workspace"))],
            Command::CreateWindow {
                workspace_id: "workspace".into(),
                expected_revision: 100,
                name: None,
                spec: spec(),
            },
        );
        assert_eq!(result.unwrap().selection.unwrap().window, "window");
        assert_eq!(requests.len(), 1);
        assert!(matches!(&requests[0].operation, Operation::WindowCreate {
            workspace_id, expected_revision: 100, name: None, ..
        } if workspace_id == "workspace"));
    }

    #[test]
    fn creation_responses_must_confirm_all_identities_membership_and_projection() {
        for malformed in 0..6 {
            let mut snapshot = registry(10, workspace(1, Some(leaf("new"))), &["new"]);
            let mut reply = RegistryResult {
                registry: snapshot.clone(),
                pane_id: Some("new".into()),
                workspace_id: Some("workspace".into()),
                window_id: Some("window".into()),
                created_panes: Default::default(),
            };
            match malformed {
                0 => reply.workspace_id = None,
                1 => reply.window_id = None,
                2 => reply.pane_id = None,
                3 => snapshot.panes[0].window_id = Some("foreign".into()),
                4 => snapshot.workspaces[0].windows[0].layout = None,
                _ => snapshot.panes[0].workspace_id = Some("foreign".into()),
            }
            reply.registry = snapshot;
            let (result, requests) = scripted(
                vec![Some(Response {
                    version: PROTOCOL_VERSION,
                    failure: None,
                    ok: true,
                    result: Some(serde_json::to_value(reply).unwrap().into()),
                    error: None,
                })],
                Command::Create {
                    name: Some("fresh".into()),
                    spec: spec(),
                },
            );
            assert!(transport::outcome_unknown(&result.unwrap_err()));
            assert_eq!(
                requests.len(),
                1,
                "atomic creation must not trigger a second mutation"
            );
        }
    }

    #[test]
    fn spawn_rejects_foreign_membership_or_absent_projection_before_selecting() {
        for malformed in 0..3 {
            let mut snapshot = registry(20, workspace(8, Some(leaf("new"))), &["new"]);
            match malformed {
                0 => snapshot.panes[0].workspace_id = Some("foreign".into()),
                1 => snapshot.panes[0].window_id = Some("foreign".into()),
                _ => snapshot.workspaces[0].windows[0].layout = None,
            }
            let (result, requests) = scripted(
                vec![success(snapshot, Some("new"), None)],
                spawn_command(workspace(7, Some(leaf("old")))),
            );
            let error = result.unwrap_err();
            assert!(transport::outcome_unknown(&error));
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn atomic_spawn_response_must_include_target_workspace_before_selecting() {
        let mut incomplete = registry(30, workspace(8, Some(leaf("new"))), &["old", "new"]);
        incomplete.workspaces.clear();
        let (result, requests) = scripted(
            vec![success(incomplete, Some("new"), None)],
            spawn_command(workspace(7, Some(leaf("old")))),
        );
        assert!(transport::outcome_unknown(&result.unwrap_err()));
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn lost_first_mutation_reply_stops_without_claiming_creation_succeeded() {
        for command in [
            spawn_command(workspace(7, Some(leaf("old")))),
            reopen_command(),
            Command::Create {
                name: Some("fresh".into()),
                spec: spec(),
            },
            Command::CreateWindow {
                workspace_id: "workspace".into(),
                expected_revision: 100,
                name: None,
                spec: spec(),
            },
            Command::Single(Operation::WindowClose {
                workspace_id: "workspace".into(),
                window_id: "window".into(),
                expected_revision: 7,
            }),
            Command::Single(Operation::WorkspaceClose {
                workspace_id: "workspace".into(),
                expected_revision: 100,
            }),
            Command::Single(Operation::PaneTerminate {
                pane_id: "old".into(),
            }),
        ] {
            let (result, requests) = scripted(vec![None], command);
            let error = result.unwrap_err();
            assert!(transport::outcome_unknown(&error), "{error:#}");
            assert!(!format!("{error:#}").contains("already succeeded"));
            assert_eq!(requests.len(), 1);
        }
    }
}
