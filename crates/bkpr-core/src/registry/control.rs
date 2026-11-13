//! Authenticated agent control admission and owned headless terminal access.
use super::*;
use bkpr_protocol::control::{ControlReply, InspectQuery, TerminalReadQuery, mutation_receipt};

pub(super) struct ControlOrigin<'a> {
    pub launch: &'a crate::harness::Launch,
    pub peer: &'a crate::harness::Peer,
    pub workspace: Option<String>,
    pub operation: Operation,
    pub lease: crate::ControlLease,
    pub reply: ControlReply,
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
        reply: ControlReply,
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
            reply,
        };
        self.observe_control(Some(&origin))?;
        {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(Some(&origin))?;
        }
        let result = self
            .request_for_origin(Request::new(operation), None, Some(&origin))?
            .into_json()?;
        let result = scope_control_result(result, origin.workspace.as_deref());
        match reply {
            ControlReply::Receipt => mutation_receipt(&origin.operation, result),
            ControlReply::Full => Ok(result),
        }
    }

    pub(super) fn control_inspect(
        &self,
        query: &InspectQuery,
        origin: Option<&ControlOrigin<'_>>,
    ) -> Result<Value> {
        self.observe_control(origin)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        self.authorize_control(origin)?;
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        inspect_page(
            &state,
            query,
            origin.and_then(|origin| origin.workspace.as_deref()),
        )
    }

    pub(super) fn control_pane_read(
        &self,
        pane: &Pane,
        query: &TerminalReadQuery,
        origin: Option<&ControlOrigin<'_>>,
        client: Option<Arc<sessions::ClientLease>>,
    ) -> Result<Payload> {
        // Use exactly the established owned-view admission and cleanup path.
        self.control_pane_request_inner(
            pane,
            PaneRequest::Poll {
                view_id: String::new(),
                after: 0,
                frame_after: if query.text { 0 } else { u64::MAX },
                resources: Vec::new(),
                snapshot_after: 0,
                wait_ms: 0,
                row_deltas: false,
                scene_encoding: None,
            },
            origin,
            client,
            query,
        )
    }

    pub(super) fn control_pane_request(
        &self,
        pane: &Pane,
        request: PaneRequest,
        origin: Option<&ControlOrigin<'_>>,
        client: Option<Arc<sessions::ClientLease>>,
    ) -> Result<Payload> {
        let query = TerminalReadQuery {
            text: !matches!(request, PaneRequest::Inspect),
            ..Default::default()
        };
        self.control_pane_request_inner(pane, request, origin, client, &query)
    }

    fn control_pane_request_inner(
        &self,
        pane: &Pane,
        mut request: PaneRequest,
        origin: Option<&ControlOrigin<'_>>,
        client: Option<Arc<sessions::ClientLease>>,
        query: &TerminalReadQuery,
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
            let result = terminal_viewport(pane, query)?;
            if let Some(client) = &client {
                client.ensure_active()?;
            }
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(origin)?;
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
                let read_query = TerminalReadQuery {
                    text: frame_after != u64::MAX,
                    ..query.clone()
                };
                let result = terminal_viewport(pane, &read_query)?;
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
                    let _transaction = self.inner.transaction.lock().unwrap();
                    self.authorize_control(origin)?;
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

fn terminal_viewport(pane: &Pane, query: &TerminalReadQuery) -> Result<Value> {
    use bkpr_protocol::control::{TerminalCursor, TerminalReadResult};
    let (mut result, frame) = {
        let published = pane.shared.published.lock().unwrap();
        (
            TerminalReadResult {
                pane_id: pane.shared.pane_id.clone(),
                pid: published.pid,
                revision: published.scene_revision,
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
                offset: query.offset.unwrap_or(0),
                next_offset: None,
                total_bytes: 0,
                start_row: 0,
                total_rows: published.frame.as_ref().map_or(0, |frame| frame.rows.len()),
                complete: true,
            },
            published.frame.clone(),
        )
    };
    ensure!(
        query
            .expected_revision
            .is_none_or(|revision| revision == result.revision),
        "stale terminal revision; current_revision={}",
        result.revision
    );
    if query.text {
        let rows = frame
            .as_ref()
            .map_or(&[][..], |frame| frame.rows.as_slice());
        let (start_row, total_bytes, text) = viewport_text(rows, query)?;
        result.start_row = start_row;
        result.total_bytes = total_bytes;
        let next = result.offset + text.len();
        result.complete = next == total_bytes;
        result.next_offset = (!result.complete).then_some(next);
        result.text = Some(text);
    }
    Ok(serde_json::to_value(result)?)
}

/// Project immutable canonical rows: (first row, selected UTF-8 bytes, page).
/// The live reader and typed-frame regression boundary use the same projection.
pub(super) fn viewport_text(
    rows: &[bkpr_protocol::Row],
    query: &TerminalReadQuery,
) -> Result<(usize, usize, String)> {
    let start_row = rows
        .len()
        .saturating_sub(query.tail_rows.unwrap_or(rows.len()));
    let mut page = TextPage::new(query.offset.unwrap_or(0), query.max_bytes.unwrap_or(2048));
    for (index, row) in rows.iter().enumerate().skip(start_row) {
        if index != start_row {
            page.push("\n");
        }
        // Hold only the count of trailing padding, not a second full row.
        let mut spaces = 0usize;
        for cell in &row.cells {
            if cell.width == 0 {
                continue;
            }
            if cell.text.is_empty() || cell.style.invisible {
                spaces += usize::from(cell.width);
            } else {
                for character in cell.text.chars() {
                    if character == ' ' {
                        spaces += 1;
                    } else {
                        page.spaces(spaces);
                        spaces = 0;
                        let mut buffer = [0; 4];
                        page.push(character.encode_utf8(&mut buffer));
                    }
                }
            }
        }
    }
    ensure!(
        page.offset <= page.total,
        "offset exceeds terminal text bytes; total_bytes={}",
        page.total
    );
    ensure!(page.boundary, "offset must be a UTF-8 boundary");
    Ok((start_row, page.total, page.text))
}

struct TextPage {
    offset: usize,
    budget: usize,
    total: usize,
    text: String,
    boundary: bool,
    sealed: bool,
}
impl TextPage {
    fn new(offset: usize, budget: usize) -> Self {
        Self {
            offset,
            budget,
            total: 0,
            text: String::new(),
            boundary: offset == 0,
            sealed: false,
        }
    }
    fn push(&mut self, text: &str) {
        let start = self.total;
        self.total += text.len();
        self.boundary |= self.offset == start || self.offset == self.total;
        if start < self.offset || self.sealed {
            return;
        }
        if self.text.len() + text.len() <= self.budget {
            self.text.push_str(text);
        } else {
            self.sealed = true;
        }
    }
    fn spaces(&mut self, count: usize) {
        let start = self.total;
        self.total += count;
        self.boundary |= (start..=self.total).contains(&self.offset);
        if self.sealed || self.total <= self.offset {
            return;
        }
        let available = self.total - start.max(self.offset);
        let emitted = available.min(self.budget - self.text.len());
        self.text.extend(std::iter::repeat_n(' ', emitted));
        self.sealed = emitted < available;
    }
}

fn inspect_page<'a>(state: &'a State, query: &InspectQuery, scope: Option<&str>) -> Result<Value> {
    query.validate()?;
    let registry = &state.record.registry;
    let workspace_visible = |id: &str| {
        scope.is_none_or(|scope| scope == id)
            && query
                .workspace_id
                .as_deref()
                .is_none_or(|filter| filter == id)
    };
    // Cursor keys are stable elementary identity, not the volatile registry revision.
    // Enumerate references only: never clone argv/project/layout for default pages.
    enum Item<'b> {
        Workspace(&'b Workspace),
        Window(&'b Workspace, &'b Window),
        Pane(&'b PaneInfo),
    }
    let limit = query.limit.unwrap_or(10);
    let after = query.after.as_deref().map(|cursor| {
        let (kind, id) = cursor.split_once(':').expect("validated cursor");
        (
            match kind {
                "pane" => 0,
                "window" => 1,
                _ => 2,
            },
            id,
        )
    });
    // Keep at most one page plus a continuation witness, regardless of retained
    // history size. Keys borrow native IDs; only the returned cursor is copied.
    let mut candidates = std::collections::BTreeMap::<(u8, &'a str), Item<'a>>::new();
    let mut add = |kind: u8, id: &'a str, item: Item<'a>| {
        let key = (kind, id);
        if after.is_some_and(|after| key <= after) {
            return;
        }
        if candidates.len() > limit
            && candidates
                .last_key_value()
                .is_some_and(|(last, _)| key >= *last)
        {
            return;
        }
        candidates.insert(key, item);
        if candidates.len() > limit + 1 {
            candidates.pop_last();
        }
    };
    for workspace in &registry.workspaces {
        if !workspace_visible(&workspace.id) {
            continue;
        }
        if query.window_id.is_none() && query.pane_id.is_none() {
            add(2, &workspace.id, Item::Workspace(workspace));
        }
        for window in &workspace.windows {
            if query.window_id.as_ref().is_some_and(|id| id != &window.id) {
                continue;
            }
            if query.pane_id.is_none() {
                add(1, &window.id, Item::Window(workspace, window));
            }
        }
    }
    for pane in &registry.panes {
        if scope.is_some_and(|scope| pane.workspace_id.as_deref() != Some(scope))
            || query
                .workspace_id
                .as_ref()
                .is_some_and(|id| pane.workspace_id.as_ref() != Some(id))
            || query
                .window_id
                .as_ref()
                .is_some_and(|id| pane.window_id.as_ref() != Some(id))
            || query.pane_id.as_ref().is_some_and(|id| id != &pane.id)
            || (!query.include_retired
                && (pane.workspace_id.is_none() || pane.status == PaneStatus::Exited))
        {
            continue;
        }
        add(0, &pane.id, Item::Pane(pane));
    }
    let has_more = candidates.len() > limit;
    let mut items = Vec::with_capacity(candidates.len().min(limit));
    let mut last = None;
    for ((kind, id), item) in candidates.into_iter().take(limit) {
        let value = match item {
            Item::Workspace(workspace) => {
                let mut value = json!({"kind":"workspace","id":workspace.id,"workspace_id":workspace.id,"workspace_revision":workspace.revision});
                if query.detail {
                    value["name"] = json!(workspace.name);
                }
                value
            }
            Item::Window(workspace, window) => {
                let mut value = json!({"kind":"window","id":window.id,"workspace_id":workspace.id,"workspace_revision":workspace.revision,"window_id":window.id,"window_revision":window.revision});
                if query.detail {
                    value["name"] = json!(window.name);
                    value["layout"] = json!(window.layout);
                }
                value
            }
            Item::Pane(pane) => {
                let mut value = json!({"kind":"pane","id":pane.id,"pane_id":pane.id,"workspace_id":pane.workspace_id,"window_id":pane.window_id,"status":pane.status});
                if let Some(workspace) = registry
                    .workspaces
                    .iter()
                    .find(|owner| Some(&owner.id) == pane.workspace_id.as_ref())
                {
                    value["workspace_revision"] = json!(workspace.revision);
                    if let Some(window) = workspace
                        .windows
                        .iter()
                        .find(|window| Some(&window.id) == pane.window_id.as_ref())
                    {
                        value["window_revision"] = json!(window.revision);
                    }
                }
                if let Some(live) = state.live.get(&pane.id) {
                    value["terminal_revision"] =
                        json!(live.shared.published.lock().unwrap().scene_revision);
                }
                if let Some(harness) = &pane.harness {
                    value["harness"] = json!({"kind":harness.kind,"launch_id":harness.managed.as_ref().map(|managed| &managed.launch_id),
                        "first_prompt_submitted_at_ms":harness.managed.as_ref().and_then(|managed| managed.first_prompt_submitted_at_ms)});
                }
                value["harness_status"] = json!(
                    pane.harness_status
                        .as_ref()
                        .map(|status| json!({"state":status.state,"progress":status.progress}))
                );
                value["harness_status_changed_at_ms"] = json!(pane.harness_status_changed_at_ms);
                if query.detail {
                    let mut detail = serde_json::to_value(pane)?;
                    if let Some(title) = detail["title"].as_str() {
                        let mut title = title.to_owned();
                        project_title(state, &pane.id, &mut title);
                        detail["title"] = json!(title);
                    }
                    value["detail"] = detail;
                }
                value
            }
        };
        last = Some(format!(
            "{}:{id}",
            match kind {
                0 => "pane",
                1 => "window",
                _ => "workspace",
            }
        ));
        items.push(value);
    }
    Ok(
        json!({"registry_revision":registry.revision,"items":items,"next_cursor":if has_more { last } else { None },"has_more":has_more}),
    )
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
            "{}",
            exact_id_error(
                "workspace",
                id,
                registry
                    .workspaces
                    .iter()
                    .filter(|workspace| scope.is_none_or(|scope| scope == workspace.id))
                    .map(|workspace| workspace.id.as_str())
            )
        );
        Ok(())
    };
    let window = |workspace_id: &str, window_id: &str| -> Result<()> {
        workspace(workspace_id)?;
        let owner = registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .unwrap();
        ensure!(
            owner.windows.iter().any(|window| window.id == window_id),
            "{}",
            exact_id_error(
                "window",
                window_id,
                owner.windows.iter().map(|window| window.id.as_str())
            )
        );
        Ok(())
    };
    let pane = |id: &str, owner: Option<(&str, &str)>| -> Result<()> {
        let pane = registry
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .ok_or_else(|| {
                exact_id_error(
                    "pane",
                    id,
                    registry
                        .panes
                        .iter()
                        .filter(|pane| {
                            scope.is_none_or(|scope| pane.workspace_id.as_deref() == Some(scope))
                        })
                        .filter(|pane| {
                            owner.is_none_or(|(workspace, window)| {
                                pane.workspace_id.as_deref() == Some(workspace)
                                    && pane.window_id.as_deref() == Some(window)
                            })
                        })
                        .map(|pane| pane.id.as_str()),
                )
            })?;
        if scope.is_some() && pane.workspace_id.is_none() {
            bail!(
                "pane is retired or unowned with no workspace membership; explicit session/all scope is required; no automatic retry"
            );
        }
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
        Operation::RecoveryList => Ok(()),
        Operation::ControlInspect { query } => {
            if let Some(id) = &query.workspace_id {
                workspace(id)?;
            }
            if let Some(id) = &query.window_id {
                if let Some(workspace_id) = &query.workspace_id {
                    window(workspace_id, id)?;
                } else {
                    let windows = registry
                        .workspaces
                        .iter()
                        .filter(|owner| scope.is_none_or(|scope| scope == owner.id));
                    ensure!(
                        windows
                            .clone()
                            .any(|owner| owner.windows.iter().any(|window| window.id == *id)),
                        "{}",
                        exact_id_error(
                            "window",
                            id,
                            windows.flat_map(|owner| owner
                                .windows
                                .iter()
                                .map(|window| window.id.as_str()))
                        )
                    );
                }
            }
            if let Some(id) = &query.pane_id {
                pane(id, None)?;
            }
            Ok(())
        }
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
        | Operation::ControlPaneRead { pane_id, .. }
        | Operation::Pane { pane_id, .. } => pane(pane_id, None),
        _ => bail!("agent control operation is not supported"),
    }
}

fn exact_id_error<'a>(
    kind: &str,
    input: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> anyhow::Error {
    let matches = candidates
        .filter(|id| id.starts_with(input))
        .take(5)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        anyhow::anyhow!(
            "unknown {kind}; exact full ID required; use session.inspect with {kind}_id/window_id filters"
        )
    } else {
        anyhow::anyhow!(
            "unknown {kind}; exact full ID required; in-scope candidates: {}",
            matches.join(", ")
        )
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
