//! Authenticated agent control admission and owned headless terminal access.
use super::*;

pub(super) struct ControlOrigin<'a> {
    pub launch: &'a crate::harness::Launch,
    pub peer: &'a crate::harness::Peer,
    pub workspace: Option<String>,
    pub operation: Operation,
    pub lease: crate::ControlLease,
}

impl Registry {
    // Process observation can wait for the sampler; never hold a registry writer.
    pub(super) fn observe_control(&self, origin: Option<&ControlOrigin<'_>>) -> Result<()> {
        let Some(origin) = origin else { return Ok(()) };
        origin.lease.ensure_active()?;
        crate::harness::validate_callback(origin.peer, origin.launch)?;
        #[cfg(test)]
        {
            let barrier = self.inner.control_observe_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                let _ = barrier.resume.lock().unwrap().recv();
            }
        }
        let table = crate::harness_process::fresh_snapshot()?;
        let (active, _) = capture::foreground_context_in(origin.launch, &table)?;
        ensure!(active, "live foreground agent context unavailable");
        Ok(())
    }

    // Observe before acquiring the transaction; use only retained lifetime FDs
    // and current in-memory launch/target ownership under the writer.
    pub(super) fn authorize_control(&self, origin: Option<&ControlOrigin<'_>>) -> Result<()> {
        let Some(origin) = origin else { return Ok(()) };
        origin.lease.ensure_active()?;
        self.authorize_resume_origin(origin.launch)?;
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        let registry = &state.record.registry;
        if let Some(workspace) = &origin.workspace {
            let own = registry
                .panes
                .iter()
                .find(|pane| pane.id == origin.launch.grant.pane_id)
                .context("live agent pane unavailable")?;
            ensure!(
                own.workspace_id.as_ref() == Some(workspace),
                "live foreground workspace changed"
            );
        }
        validate_targets(registry, &origin.operation, origin.workspace.as_deref())
    }

    pub(super) fn request_for_control(
        &self,
        operation: Operation,
        launch: &crate::harness::Launch,
        peer: &crate::harness::Peer,
        workspace: Option<String>,
    ) -> Result<Value> {
        bkpr_protocol::control::validate_native(&operation)?;
        // Pane authorization needs only the target, not a copy of input bytes.
        let target = match &operation {
            Operation::Pane { pane_id, .. } => Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Inspect,
            },
            _ => operation.clone(),
        };
        let origin = ControlOrigin {
            launch,
            peer,
            workspace,
            operation: target,
            lease: launch.control_lease()?,
        };
        self.observe_control(Some(&origin))?;
        {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(Some(&origin))?;
        }
        let result = self
            .request_for_origin(Request::new(operation), None, Some(&origin))?
            .into_json()?;
        Ok(scope_control_result(result, origin.workspace.as_deref()))
    }

    pub(super) fn control_pane_request(
        &self,
        pane: &Pane,
        mut request: PaneRequest,
        origin: Option<&ControlOrigin<'_>>,
        client: Option<Arc<sessions::ClientLease>>,
    ) -> Result<Payload> {
        // Clone the live handle before this method; never hold registry or agent
        // mutexes while attaching, polling, writing or detaching a native view.
        if let Some(client) = &client {
            client.ensure_active()?;
        }
        self.observe_control(origin)?;
        {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(origin)?;
        }
        if matches!(request, PaneRequest::Inspect) {
            let result = terminal_viewport(pane, false)?;
            if let Some(client) = &client {
                client.ensure_active()?;
            }
            return Ok(result.into());
        }
        let input = matches!(
            request,
            PaneRequest::Input { .. } | PaneRequest::Paste { .. } | PaneRequest::Key { .. }
        );
        let size = pane
            .shared
            .published
            .lock()
            .unwrap()
            .frame
            .as_ref()
            .map(|frame| frame.size)
            .unwrap_or(Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            });
        let attach = PaneRequest::Attach {
            spec: ViewSpec {
                size,
                interactive: false,
                visible: true,
                input,
                clipboard: false,
            },
        };
        let attached = match origin {
            Some(origin) => pane.request_control(attach, &origin.lease)?,
            None => pane.request_for_client(attach, client.clone())?,
        }
        .into_json()?;
        let view_id = attached["view_id"]
            .as_str()
            .context("native attach omitted view ID")?
            .to_owned();
        let mut view = TemporaryView {
            pane: pane.clone(),
            id: Some(view_id.clone()),
            client: client.clone(),
        };
        match &mut request {
            PaneRequest::Poll { view_id: id, .. }
            | PaneRequest::Input { view_id: id, .. }
            | PaneRequest::Paste { view_id: id, .. }
            | PaneRequest::Key { view_id: id, .. } => *id = view_id,
            _ => bail!("unsupported headless terminal request"),
        }
        // Revalidate after attaching, before any terminal side effect. Detach is
        // unconditional on errors, including launch revocation and invalid input.
        self.observe_control(origin)?;
        let admitted = {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(origin)
        };
        let result = admitted.and_then(|()| {
            if let PaneRequest::Poll { frame_after, .. } = request {
                if let Some(client) = &client {
                    client.ensure_active()?;
                }
                let result = terminal_viewport(pane, frame_after != u64::MAX)?;
                if let Some(client) = &client {
                    client.ensure_active()?;
                }
                Ok(result)
            } else {
                match origin {
                    Some(origin) => pane.request_control(request, &origin.lease)?,
                    None => pane.request_for_client(request, client.clone())?,
                }
                .into_json()
            }
        });
        #[cfg(test)]
        {
            let barrier = self.inner.control_cleanup_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                let _ = barrier.entered.send(());
                let _ = barrier.resume.lock().unwrap().recv();
            }
        }
        let detached = view.detach();
        Ok(result
            .and_then(|result| {
                // A known input receipt must survive owned-view cleanup failure.
                if !input {
                    detached?;
                    if let Some(client) = &client {
                        client.ensure_active()?;
                    }
                }
                Ok(result)
            })?
            .into())
    }
}

// Every mutation, including guarded recovery, uses the same response fence.
pub(super) fn scope_control_result(mut result: Value, workspace: Option<&str>) -> Value {
    if let Some(workspace) = workspace
        && let Some(registry) = result.get_mut("registry")
    {
        if let Some(workspaces) = registry.get_mut("workspaces").and_then(Value::as_array_mut) {
            workspaces.retain(|item| item["id"].as_str() == Some(workspace));
        }
        if let Some(panes) = registry.get_mut("panes").and_then(Value::as_array_mut) {
            panes.retain(|item| item["workspace_id"].as_str() == Some(workspace));
        }
    }
    result
}

struct TemporaryView {
    pane: Pane,
    id: Option<String>,
    client: Option<Arc<sessions::ClientLease>>,
}
impl TemporaryView {
    fn detach(&mut self) -> Result<()> {
        if let Some(view_id) = self.id.take()
            && let Err(error) = self.pane.request_for_client(
                PaneRequest::Detach {
                    view_id: view_id.clone(),
                },
                self.client.clone(),
            )
        {
            // Expiry already performed the same owned cleanup. Do not turn
            // an accepted write into an error because its view is gone.
            if self
                .pane
                .shared
                .authority
                .lock()
                .unwrap()
                .views
                .contains_key(&view_id)
            {
                return Err(error);
            }
        }
        Ok(())
    }
}
impl Drop for TemporaryView {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}

fn terminal_viewport(pane: &Pane, include_text: bool) -> Result<Value> {
    use bkpr_protocol::control::{TerminalCursor, TerminalReadResult};
    let (mut result, frame) = {
        let published = pane.shared.published.lock().unwrap();
        (
            TerminalReadResult {
                pane_id: pane.shared.pane_id.clone(),
                pid: published.pid,
                revision: published.revision,
                lifecycle: published.lifecycle.clone(),
                exit_code: published.exit_code,
                error: published.error.clone(),
                size: published.frame.as_ref().map(|frame| frame.size),
                alternate_screen: published.frame.as_ref().map(|frame| frame.alternate_screen),
                cursor: published.frame.as_ref().map(|frame| TerminalCursor {
                    x: frame.cursor.x,
                    y: frame.cursor.y,
                    visible: frame.cursor.visible,
                }),
                text: None,
                images_omitted: published
                    .frame
                    .as_ref()
                    .is_some_and(|frame| !frame.images.is_empty() || !frame.placements.is_empty()),
            },
            published.frame.clone(),
        )
    };
    if include_text {
        let mut text = String::new();
        if let Some(frame) = frame {
            for (index, row) in frame.rows.iter().enumerate() {
                if index != 0 {
                    text.push('\n');
                }
                for cell in &row.cells {
                    if cell.width == 0 {
                        continue;
                    }
                    if cell.text.is_empty() || cell.style.invisible {
                        for _ in 0..cell.width {
                            text.push(' ');
                        }
                    } else {
                        text.push_str(&cell.text);
                    }
                }
            }
        }
        result.text = Some(text);
    }
    Ok(serde_json::to_value(result)?)
}

fn validate_targets(
    registry: &RegistrySnapshot,
    operation: &Operation,
    scope: Option<&str>,
) -> Result<()> {
    let workspace = |id: &str| -> Result<()> {
        ensure!(
            scope.is_none_or(|scope| scope == id),
            "target workspace is outside agent control scope"
        );
        ensure!(
            registry
                .workspaces
                .iter()
                .any(|workspace| workspace.id == id),
            "unknown workspace"
        );
        Ok(())
    };
    let window = |workspace_id: &str, window_id: &str| -> Result<()> {
        workspace(workspace_id)?;
        ensure!(
            registry
                .workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id
                    && workspace
                        .windows
                        .iter()
                        .any(|window| window.id == window_id)),
            "unknown window in target workspace"
        );
        Ok(())
    };
    let pane = |id: &str, owner: Option<(&str, &str)>| -> Result<()> {
        let pane = registry
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .context("unknown pane")?;
        ensure!(
            scope.is_none_or(|scope| pane.workspace_id.as_deref() == Some(scope)),
            "target pane is outside agent control scope"
        );
        if let Some((workspace, window)) = owner {
            ensure!(
                pane.workspace_id.as_deref() == Some(workspace)
                    && pane.window_id.as_deref() == Some(window),
                "layout pane belongs to another window"
            );
        }
        Ok(())
    };
    fn layout(node: &LayoutNode, check: &impl Fn(&str) -> Result<()>) -> Result<()> {
        match node {
            LayoutNode::Leaf { pane_id } => check(pane_id),
            LayoutNode::Split { first, second, .. } => {
                layout(first, check)?;
                layout(second, check)
            }
        }
    }
    fn spawn_layout(node: &SpawnLayoutNode, check: &impl Fn(&str) -> Result<()>) -> Result<()> {
        match node {
            SpawnLayoutNode::Existing { pane_id } => check(pane_id),
            SpawnLayoutNode::New { .. } => Ok(()),
            SpawnLayoutNode::Split { first, second, .. } => {
                spawn_layout(first, check)?;
                spawn_layout(second, check)
            }
        }
    }
    match operation {
        Operation::Inspect | Operation::RecoveryList => Ok(()),
        Operation::CiRefresh { target } => pane(&target.pane_id, None),
        Operation::WorkspaceCreate { .. } => {
            ensure!(
                scope.is_none(),
                "workspace.create requires explicit session or all scope"
            );
            Ok(())
        }
        Operation::WorkspaceRename { workspace_id, .. }
        | Operation::WorkspaceClose { workspace_id, .. }
        | Operation::WindowCreate { workspace_id, .. } => workspace(workspace_id),
        Operation::WindowRename {
            workspace_id,
            window_id,
            ..
        }
        | Operation::WindowClose {
            workspace_id,
            window_id,
            ..
        } => window(workspace_id, window_id),
        Operation::PaneReopen {
            workspace_id,
            window_id,
            pane_id,
            ..
        } => {
            window(workspace_id, window_id)?;
            pane(pane_id, Some((workspace_id, window_id)))
        }
        Operation::PaneSpawn {
            workspace_id,
            window_id,
            layout,
            ..
        } => {
            match (workspace_id, window_id) {
                (Some(workspace_id), Some(window_id)) => {
                    window(workspace_id, window_id)?;
                    if let Some(layout) = layout {
                        spawn_layout(layout, &|id| pane(id, Some((workspace_id, window_id))))?;
                    }
                }
                (None, None) => {
                    ensure!(
                        scope.is_none(),
                        "pane.create requires explicit workspace_id and window_id in workspace scope"
                    );
                    ensure!(
                        layout.is_none(),
                        "unowned pane.create cannot reference a layout"
                    );
                }
                _ => bail!("pane.create requires both workspace_id and window_id"),
            }
            Ok(())
        }
        Operation::LayoutUpdate {
            workspace_id,
            window_id,
            layout: node,
            ..
        } => {
            window(workspace_id, window_id)?;
            if let Some(node) = node {
                layout(node, &|id| pane(id, Some((workspace_id, window_id))))?;
            }
            Ok(())
        }
        Operation::PaneResume { pane_id, .. }
        | Operation::PaneTerminate { pane_id }
        | Operation::PaneForget { pane_id }
        | Operation::PaneRename { pane_id, .. }
        | Operation::Pane { pane_id, .. } => pane(pane_id, None),
        _ => bail!("agent control operation is not supported"),
    }
}

#[cfg(test)]
mod ci_scope_tests {
    use super::*;

    #[test]
    fn refresh_targets_only_an_existing_pane_in_the_admitted_workspace() {
        let registry: RegistrySnapshot = serde_json::from_value(json!({
            "revision":1, "workspaces":[],
            "panes":[{
                "id":"pane", "workspace_id":"own", "window_id":"window",
                "title":"title", "status":"running", "exit_code":null,
                "cwd":"/project", "home":"/home", "isolated_environment":false
            }]
        }))
        .unwrap();
        let mut operation = Operation::CiRefresh {
            target: CiRefreshTarget {
                pane_id: "pane".into(),
                cwd: "/project".into(),
                repository: ForgeRepository {
                    provider: ForgeProvider::Github,
                    host: "github.com".into(),
                    path: "o/r".into(),
                    web_url: "https://github.com/o/r".into(),
                },
                head_sha: "remote".into(),
                local_head_sha: "local".into(),
                checked_ref: None,
            },
        };
        assert!(validate_targets(&registry, &operation, Some("own")).is_ok());
        assert!(validate_targets(&registry, &operation, Some("other")).is_err());
        assert!(validate_targets(&registry, &operation, None).is_ok());
        let Operation::CiRefresh { target } = &mut operation else {
            unreachable!()
        };
        target.pane_id = "missing".into();
        assert!(validate_targets(&registry, &operation, None).is_err());
    }
}
