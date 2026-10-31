use crate::{controller::split_leaf, transport};
use anyhow::{Context, Result};
use bkpr_protocol::*;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum Command {
    Single(Operation),
    Spawn {
        workspace: Workspace,
        target: String,
        axis: Axis,
        spec: PaneSpawnSpec,
    },
    Create {
        name: String,
        spec: PaneSpawnSpec,
    },
}

#[derive(Debug)]
pub(crate) struct Selection {
    pub workspace: String,
    pub pane: String,
}

#[derive(Debug)]
pub(crate) struct Completion {
    pub registry: RegistrySnapshot,
    pub selection: Option<Selection>,
}

// Synchronous by design: callers must dispatch this entire sequence to a worker.
// A later failure never replays or rolls back an earlier mutation.
pub(crate) fn execute(socket: &Path, command: Command) -> Result<Completion> {
    match command {
        Command::Single(operation) => {
            let result = request(socket, operation, "operation")?;
            Ok(Completion {
                registry: result.registry,
                selection: None,
            })
        }
        Command::Spawn {
            workspace,
            target,
            axis,
            spec,
        } => spawn(socket, workspace, &target, axis, spec),
        Command::Create { name, spec } => {
            let created = request(
                socket,
                Operation::WorkspaceCreate { name },
                "workspace creation",
            )?;
            (|| {
                let id = created
                    .workspace_id
                    .context("workspace creation response omitted workspace_id")
                    .map_err(transport::mark_outcome_unknown)?;
                let workspace = created
                    .registry
                    .workspaces
                    .into_iter()
                    .find(|workspace| workspace.id == id)
                    .context("workspace creation response omitted created workspace from registry")
                    .map_err(transport::mark_outcome_unknown)?;
                spawn(socket, workspace, "", Axis::Horizontal, spec)
            })()
            .context("workspace creation already succeeded; the created workspace was not removed")
        }
    }
}

fn spawn(
    socket: &Path,
    workspace: Workspace,
    target: &str,
    axis: Axis,
    spec: PaneSpawnSpec,
) -> Result<Completion> {
    let created = request(
        socket,
        Operation::PaneSpawn {
            workspace_id: Some(workspace.id.clone()),
            expected_revision: Some(workspace.revision),
            spec,
        },
        "pane creation",
    )?;
    (|| -> Result<Completion> {
        let pane = created.pane_id.context("pane creation response omitted pane_id")
            .map_err(transport::mark_outcome_unknown)?;
        let entry = created.registry.panes.iter().find(|entry| entry.id == pane)
            .context("pane creation response omitted created pane from registry")
            .map_err(transport::mark_outcome_unknown)?;
        if entry.workspace_id.as_deref() != Some(workspace.id.as_str()) {
            return Err(transport::mark_outcome_unknown(anyhow::anyhow!(
                "pane creation response associated created pane with a different workspace"
            )));
        }
        let revision = created.registry.workspaces.iter()
            .find(|entry| entry.id == workspace.id)
            .context("pane creation response omitted target workspace from registry")
            .map_err(transport::mark_outcome_unknown)?.revision;
        let placed = request(
            socket,
            Operation::LayoutUpdate {
                workspace_id: workspace.id.clone(),
                expected_revision: revision,
                // Never substitute a newer layout for the user's captured intent.
                layout: split_leaf(workspace.layout, target, &pane, axis),
            },
            "layout update",
        )?;
        placed.registry.workspaces.iter().find(|entry| entry.id == workspace.id)
            .context("layout update response omitted target workspace from registry")
            .map_err(transport::mark_outcome_unknown)?;
        placed.registry.panes.iter().find(|entry| entry.id == pane)
            .context("layout update response omitted created pane from registry")
            .map_err(transport::mark_outcome_unknown)?;
        Ok(Completion {
            registry: placed.registry,
            selection: Some(Selection { workspace: workspace.id, pane }),
        })
    })().context("pane creation already succeeded; its process remains registry-owned even if layout placement was not confirmed")
}

fn request(socket: &Path, operation: Operation, phase: &str) -> Result<RegistryResult> {
    let value = transport::checked(socket, operation)?;
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
        io::{BufRead, BufReader, Write},
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
            revision,
            layout,
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
                    title: (*id).into(),
                    status: PaneStatus::Running,
                    exit_code: None,
                    cwd: "/tmp".into(),
                    home: "/tmp".into(),
                    isolated_environment: false,
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
                })
                .unwrap(),
            ),
            error: None,
        })
    }

    fn rejection(message: &str) -> Option<Response> {
        Some(Response {
            version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some(message.into()),
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
                        let mut bytes = Vec::new();
                        BufReader::new(&mut stream)
                            .read_until(b'\n', &mut bytes)
                            .unwrap();
                        let request: Request = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(request.version, PROTOCOL_VERSION);
                        requests.push(request);
                        if let Some(reply) = replies
                            .next()
                            .unwrap_or_else(|| rejection("unexpected mutation replay"))
                        {
                            serde_json::to_writer(&mut stream, &reply).unwrap();
                            stream.write_all(b"\n").unwrap();
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
        let result = execute(&socket, command);
        done.send(()).unwrap();
        (result, server.join().unwrap())
    }

    fn spawn_command(workspace: Workspace) -> Command {
        Command::Spawn {
            workspace,
            target: "old".into(),
            axis: Axis::Vertical,
            spec: spec(),
        }
    }

    #[test]
    fn spawn_uses_captured_layout_and_cas_then_returned_workspace_revision() {
        let old_layout = split(Axis::Horizontal, 325, leaf("old"), leaf("other"));
        let expected_layout = split(
            Axis::Horizontal,
            325,
            split(Axis::Vertical, 500, leaf("old"), leaf("new")),
            leaf("other"),
        );
        // The reply deliberately has a different layout and unrelated global
        // revision: neither may replace captured layout/post-spawn workspace CAS.
        let replies = vec![
            success(
                registry(
                    900,
                    workspace(18, Some(leaf("other"))),
                    &["old", "other", "new"],
                ),
                Some("new"),
                None,
            ),
            success(
                registry(
                    901,
                    workspace(19, Some(expected_layout.clone())),
                    &["old", "other", "new"],
                ),
                None,
                None,
            ),
        ];
        let (result, requests) = scripted(replies, spawn_command(workspace(7, Some(old_layout))));
        let completion = result.unwrap();
        assert_eq!(requests.len(), 2);
        match &requests[0].operation {
            Operation::PaneSpawn {
                workspace_id,
                expected_revision,
                ..
            } => {
                assert_eq!(workspace_id.as_deref(), Some("workspace"));
                assert_eq!(*expected_revision, Some(7));
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        match &requests[1].operation {
            Operation::LayoutUpdate {
                workspace_id,
                expected_revision,
                layout,
            } => {
                assert_eq!(workspace_id, "workspace");
                assert_eq!(*expected_revision, 18);
                assert_eq!(layout.as_ref(), Some(&expected_layout));
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        assert_eq!(completion.registry.revision, 901);
        let selected = completion.selection.unwrap();
        assert_eq!(selected.workspace, "workspace");
        assert_eq!(selected.pane, "new");
    }

    #[test]
    fn failed_or_lost_placement_keeps_created_pane_and_never_replays() {
        for (reply, unknown) in [(rejection("revision conflict"), false), (None, true)] {
            let (result, requests) = scripted(
                vec![
                    success(
                        registry(20, workspace(8, Some(leaf("old"))), &["old", "new"]),
                        Some("new"),
                        None,
                    ),
                    reply,
                ],
                spawn_command(workspace(7, Some(leaf("old")))),
            );
            let error = result.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("pane creation already succeeded"),
                "{message}"
            );
            assert!(message.contains("remains registry-owned"), "{message}");
            assert_eq!(transport::outcome_unknown(&error), unknown, "{message}");
            assert_eq!(
                requests.len(),
                2,
                "must not replay spawn/layout or terminate the new pane"
            );
            assert!(matches!(
                &requests[1].operation,
                Operation::LayoutUpdate { .. }
            ));
        }
    }

    #[test]
    fn create_later_failures_report_every_successful_prior_phase() {
        for lost_phase in [1, 2] {
            let mut replies = vec![success(
                registry(10, workspace(1, None), &[]),
                None,
                Some("workspace"),
            )];
            if lost_phase == 2 {
                replies.push(success(
                    registry(11, workspace(2, None), &["new"]),
                    Some("new"),
                    None,
                ));
            }
            replies.push(None);
            let (result, requests) = scripted(
                replies,
                Command::Create {
                    name: "fresh".into(),
                    spec: spec(),
                },
            );
            let error = result.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("workspace creation already succeeded"),
                "{message}"
            );
            assert_eq!(
                message.contains("pane creation already succeeded"),
                lost_phase == 2,
                "{message}"
            );
            assert!(transport::outcome_unknown(&error), "{message}");
            assert_eq!(requests.len(), lost_phase + 1);
        }
    }

    #[test]
    fn omitted_spawn_ids_or_registry_entries_report_creation_without_placement() {
        for (pane_id, workspaces, panes, expected) in [
            (
                None,
                vec![workspace(8, None)],
                vec!["new"],
                "omitted pane_id",
            ),
            (
                Some("new"),
                Vec::new(),
                vec!["new"],
                "omitted target workspace",
            ),
            (
                Some("new"),
                vec![workspace(8, None)],
                Vec::new(),
                "omitted created pane",
            ),
            (
                Some("different"),
                vec![workspace(8, None)],
                vec!["new"],
                "omitted created pane",
            ),
        ] {
            let mut snapshot = registry(20, workspace(8, None), &panes);
            snapshot.workspaces = workspaces;
            let (result, requests) = scripted(
                vec![success(snapshot, pane_id, None)],
                spawn_command(workspace(7, Some(leaf("old")))),
            );
            let error = result.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("pane creation already succeeded"),
                "{message}"
            );
            assert!(message.contains(expected), "{message}");
            assert!(transport::outcome_unknown(&error), "{message}");
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn omitted_created_workspace_id_or_entry_does_not_spawn() {
        for (id, include_workspace, expected) in [
            (None, true, "omitted workspace_id"),
            (Some("workspace"), false, "omitted created workspace"),
            (Some("different"), true, "omitted created workspace"),
        ] {
            let mut snapshot = registry(10, workspace(1, None), &[]);
            if !include_workspace {
                snapshot.workspaces.clear();
            }
            let (result, requests) = scripted(
                vec![success(snapshot, None, id)],
                Command::Create {
                    name: "fresh".into(),
                    spec: spec(),
                },
            );
            let error = result.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("workspace creation already succeeded"),
                "{message}"
            );
            assert!(message.contains(expected), "{message}");
            assert!(transport::outcome_unknown(&error), "{message}");
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn mismatched_spawn_membership_does_not_attempt_placement() {
        let mut snapshot = registry(20, workspace(8, None), &["new"]);
        snapshot.panes[0].workspace_id = Some("different".into());
        let (result, requests) = scripted(
            vec![success(snapshot, Some("new"), None)],
            spawn_command(workspace(7, Some(leaf("old")))),
        );
        let error = result.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("pane creation already succeeded"),
            "{message}"
        );
        assert!(message.contains("different workspace"), "{message}");
        assert!(transport::outcome_unknown(&error), "{message}");
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn malformed_success_reports_completed_mutation_without_replay() {
        for (command, phase) in [
            (spawn_command(workspace(7, None)), "pane creation"),
            (
                Command::Create {
                    name: "fresh".into(),
                    spec: spec(),
                },
                "workspace creation",
            ),
            (
                Command::Single(Operation::PaneTerminate {
                    pane_id: "old".into(),
                }),
                "operation",
            ),
        ] {
            let malformed = Some(Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(serde_json::json!({})),
                error: None,
            });
            let (result, requests) = scripted(vec![malformed], command);
            let error = result.unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains(&format!("{phase} already succeeded")),
                "{message}"
            );
            assert!(
                message.contains("omitted a valid registry result"),
                "{message}"
            );
            assert!(transport::outcome_unknown(&error), "{message}");
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn layout_response_omissions_keep_partial_success_context() {
        let mut incomplete = registry(30, workspace(9, Some(leaf("new"))), &["old", "new"]);
        incomplete.workspaces.clear();
        let prior = success(
            registry(20, workspace(8, Some(leaf("old"))), &["old", "new"]),
            Some("new"),
            None,
        );
        let (result, requests) = scripted(
            vec![prior, success(incomplete, None, None)],
            spawn_command(workspace(7, Some(leaf("old")))),
        );
        let error = result.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("pane creation already succeeded"),
            "{message}"
        );
        assert!(
            message.contains("layout update response omitted target workspace"),
            "{message}"
        );
        assert!(transport::outcome_unknown(&error), "{message}");
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn lost_first_mutation_reply_stops_without_claiming_creation_succeeded() {
        for command in [
            spawn_command(workspace(7, Some(leaf("old")))),
            Command::Create {
                name: "fresh".into(),
                spec: spec(),
            },
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
