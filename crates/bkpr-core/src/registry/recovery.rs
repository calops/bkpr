//! Explicit native resume intent. This store never grants live capture or inbox authority.
use super::*;
use std::os::unix::ffi::OsStrExt;

#[derive(Clone)]
pub(super) struct ResumeAttempt {
    pub source: String,
    pub expected: HarnessRecovery,
    pub workspace: Option<String>,
}

pub(super) fn validate_id(kind: HarnessKind, id: &str) -> Result<()> {
    ensure!(
        kind.is_managed(),
        "harness does not support managed recovery"
    );
    ensure!(
        !id.is_empty()
            && id.len() <= 1024
            && id.trim() == id
            && !id.starts_with('-')
            && !id.chars().any(char::is_control),
        "invalid native conversation ID"
    );
    if kind == HarnessKind::Agy {
        ensure!(
            Uuid::parse_str(id)?.to_string() == id,
            "AGY conversation must be a canonical UUID"
        );
    }
    Ok(())
}
pub(super) fn validate(descriptor: &HarnessRecovery) -> Result<()> {
    validate_id(descriptor.kind, &descriptor.conversation_id)?;
    let bytes = descriptor.cwd.as_os_str().as_bytes();
    ensure!(
        descriptor.cwd.is_absolute() && bytes.len() <= 16 * 1024 && !bytes.contains(&0),
        "invalid recovery working directory"
    );
    Ok(())
}
fn same_conversation(a: &HarnessRecovery, b: &HarnessRecovery) -> bool {
    a.kind == b.kind && a.conversation_id == b.conversation_id
}
fn bound(captures: &crate::harness::Captures, descriptor: &HarnessRecovery) -> bool {
    captures.launches.values().any(|launch| {
        launch.grant.kind == descriptor.kind
            && launch
                .managed
                .conversation
                .as_ref()
                .is_some_and(|native| native.id == descriptor.conversation_id)
    })
}
pub(super) fn check_resume(
    state: &State,
    captures: &crate::harness::Captures,
    attempt: &ResumeAttempt,
) -> Result<()> {
    check_source(state, attempt)?;
    ensure!(
        !bound(captures, &attempt.expected),
        "native conversation is already running"
    );
    ensure!(
        !state
            .resuming
            .values()
            .any(|other| same_conversation(&other.expected, &attempt.expected)),
        "native conversation resume already in progress"
    );
    Ok(())
}
fn check_source(state: &State, attempt: &ResumeAttempt) -> Result<()> {
    ensure!(
        state.record.recoveries.get(&attempt.source) == Some(&attempt.expected),
        "stale or missing recovery binding"
    );
    let pane = state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == attempt.source)
        .context("unknown recovery pane")?;
    ensure!(
        attempt
            .workspace
            .as_ref()
            .is_none_or(|workspace| pane.workspace_id.as_ref() == Some(workspace)),
        "recovery pane is outside authenticated workspace scope"
    );
    ensure!(
        pane.status != PaneStatus::Running
            && state.live.get(&pane.id).is_none_or(|pane| pane
                .shared
                .finished
                .load(std::sync::atomic::Ordering::Acquire)),
        "recovery pane execution is still running"
    );
    Ok(())
}
pub(super) fn check_resume_reserved(
    state: &State,
    attempt: &ResumeAttempt,
    id: &str,
) -> Result<()> {
    check_source(state, attempt)?;
    ensure!(
        state.resuming.get(id).is_some_and(
            |current| current.source == attempt.source && current.expected == attempt.expected
        ),
        "native resume reservation retired"
    );
    Ok(())
}

impl Registry {
    pub(super) fn recovery_candidates(&self) -> Result<Vec<RecoveryCandidate>> {
        let configured = self.inner.spawn_environment.lock().unwrap().is_some();
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        let captures = self.inner.captures.lock().unwrap();
        let running = state
            .record
            .registry
            .panes
            .iter()
            .filter(|pane| pane.status == PaneStatus::Running)
            .count();
        let pending_workspaces = state
            .spawn_workspaces
            .iter()
            .filter(|id| {
                !state
                    .record
                    .registry
                    .workspaces
                    .iter()
                    .any(|workspace| &workspace.id == *id)
            })
            .count();
        Ok(state
            .record
            .recoveries
            .iter()
            .map(|(id, descriptor)| {
                let pane = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|pane| &pane.id == id)
                    .expect("validated recovery pane");
                let (status, reason) = if bound(&captures, descriptor)
                    || state
                        .resuming
                        .values()
                        .any(|other| same_conversation(&other.expected, descriptor))
                {
                    (
                        RecoveryStatus::Live,
                        Some("native conversation is running or resume is pending".to_owned()),
                    )
                } else {
                    let ready = (|| -> Result<()> {
                        ensure!(
                            !state.closing && !self.session_end_requested(),
                            SessionClosing("session is closing")
                        );
                        ensure!(configured, "managed harnesses are not configured");
                        ensure!(
                            pane.status != PaneStatus::Running
                                && state.live.get(id).is_none_or(|pane| pane
                                    .shared
                                    .finished
                                    .load(std::sync::atomic::Ordering::Acquire)),
                            "recovery pane execution is still running"
                        );
                        ensure!(
                            state.record.registry.panes.len() + state.spawning < MAX_PANE_RECORDS,
                            "pane record limit"
                        );
                        ensure!(running + state.spawning < MAX_LIVE_PANES, "live pane limit");
                        if let Some(workspace_id) = &pane.workspace_id {
                            workspace_available(&state, workspace_id)?;
                        } else {
                            ensure!(
                                state.closing_workspaces.is_empty()
                                    && state.record.registry.workspaces.len() + pending_workspaces
                                        < MAX_WORKSPACES,
                                "workspace limit or close in progress"
                            );
                        }
                        Ok(())
                    })();
                    match ready {
                        Ok(()) => (RecoveryStatus::Recoverable,
                            state.failed_restores.contains(id).then(||
                                format!("automatic native resume exited before verified binding (exit {:?}); saved intent retained", pane.exit_code))),
                        Err(error) => (RecoveryStatus::Unavailable, Some(error.to_string())),
                    }
                };
                RecoveryCandidate {
                    pane_id: id.clone(),
                    workspace_id: pane.workspace_id.clone(),
                    window_id: pane.window_id.clone(),
                    window_revision: pane
                        .workspace_id
                        .as_deref()
                        .zip(pane.window_id.as_deref())
                        .and_then(|(workspace, window)| {
                            find_window(&state.record.registry, workspace, window).ok()
                        })
                        .map(|window| window.revision),
                    title: pane.title.clone(),
                    harness: descriptor.clone(),
                    status,
                    reason,
                }
            })
            .collect())
    }
    pub(super) fn recovery_list(&self) -> Result<Value> {
        Ok(
            json!({"policy":"current_native_configuration", "recoveries":self.recovery_candidates()?}),
        )
    }
    pub(super) fn resume(&self, source: &str, expected: HarnessRecovery) -> Result<Value> {
        self.resume_scoped(source, expected, None, None)
    }
    pub(super) fn resume_scoped(
        &self,
        source: &str,
        expected: HarnessRecovery,
        workspace: Option<String>,
        origin: Option<(&crate::harness::Launch, &crate::harness::Peer)>,
    ) -> Result<Value> {
        validate(&expected)?;
        let configured = self.inner.spawn_environment.lock().unwrap().is_some();
        ensure!(configured, "managed harnesses are not configured");
        let (target, home, isolated_environment, title) = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            let attempt = ResumeAttempt {
                source: source.to_owned(),
                expected: expected.clone(),
                workspace: workspace.clone(),
            };
            check_resume(&state, &self.inner.captures.lock().unwrap(), &attempt)?;
            let pane = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == source)
                .context("unknown recovery pane")?;
            let target = match (&pane.workspace_id, &pane.window_id) {
                (Some(workspace_id), Some(window_id)) => {
                    let window = find_window(&state.record.registry, workspace_id, window_id)?;
                    if window
                        .layout
                        .as_ref()
                        .is_some_and(|layout| layout_references(layout, source))
                    {
                        SpawnTarget::Reopen {
                            workspace_id: workspace_id.clone(),
                            window_id: window_id.clone(),
                            expected_revision: window.revision,
                            pane_id: source.to_owned(),
                        }
                    } else {
                        SpawnTarget::Append {
                            workspace_id: Some(workspace_id.clone()),
                            window_id: Some(window_id.clone()),
                            expected_revision: Some(window.revision),
                        }
                    }
                }
                (None, None) => SpawnTarget::CreateWorkspace {
                    workspace_id: Uuid::new_v4().to_string(),
                    window_id: Uuid::new_v4().to_string(),
                    name: None,
                    bootstrap: false,
                },
                _ => bail!("invalid recovery pane membership"),
            };
            (
                target,
                pane.home.clone(),
                pane.isolated_environment,
                pane.title.clone(),
            )
        };
        let spec = self.native_resume_spec(
            &expected,
            home,
            isolated_environment,
            title,
            Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
        )?;
        let result = self.spawn_inner(
            target,
            spec,
            Some(ResumeAttempt {
                source: source.to_owned(),
                expected,
                workspace,
            }),
            origin,
        )?;
        let mut result = result;
        result["policy"] = json!("current_native_configuration");
        result["started"] = json!(true);
        result["binding"] = json!("pending_native_binding");
        result["source_pane_id"] = json!(source);
        Ok(result)
    }
    pub(super) fn native_resume_spec(
        &self,
        expected: &HarnessRecovery,
        home: PathBuf,
        isolated_environment: bool,
        title: String,
        size: Size,
    ) -> Result<PaneSpawnSpec> {
        validate(expected)?;
        let environment = self
            .inner
            .spawn_environment
            .lock()
            .unwrap()
            .clone()
            .context("managed harnesses are not configured")?;
        let flag = match expected.kind {
            HarnessKind::Claude | HarnessKind::Omp => "--resume",
            HarnessKind::Codex => "resume",
            HarnessKind::Agy => "--conversation",
            _ => bail!("harness does not support managed recovery"),
        };
        Ok(PaneSpawnSpec {
            argv: vec![
                environment
                    .executable
                    .to_str()
                    .context("configured bkpr executable is not UTF-8")?
                    .to_owned(),
                "run".into(),
                expected.kind.as_str().into(),
                flag.into(),
                expected.conversation_id.clone(),
            ],
            cwd: expected.cwd.clone(),
            home,
            isolated_environment,
            title: Some(title),
            size,
        })
    }
    // Caller owns the durable transaction. Only authenticated lifecycle paths call this.
    pub(super) fn recovery_begin(&self, pane_id: &str) -> Result<()> {
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            if !state.record.recoveries.contains_key(pane_id)
                || state
                    .resuming
                    .get(pane_id)
                    .is_some_and(|attempt| attempt.source == pane_id)
            {
                return Ok(());
            }
            state.record.clone()
        };
        next.recoveries.remove(pane_id);
        next.automatic.remove(pane_id);
        next.registry.revision = next_revision(next.registry.revision)?;
        commit(&self.inner, next, |_| {})?;
        Ok(())
    }
    pub(super) fn recovery_bind(
        &self,
        launch: &crate::harness::Launch,
        descriptor: HarnessRecovery,
        active: bool,
    ) -> Result<()> {
        validate(&descriptor)?;
        let pane_id = &launch.grant.pane_id;
        let (mut next, source) = {
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            // This authenticated binding supersedes earlier sampled role/CWD,
            // including when its durable descriptor is otherwise unchanged.
            state.pending_directories.remove(pane_id);
            let source = state
                .resuming
                .get(pane_id)
                .map(|attempt| attempt.source.clone());
            if state.record.recoveries.get(pane_id) == Some(&descriptor)
                && source.is_none()
                && state.record.automatic.get(pane_id) == active.then_some(&descriptor)
            {
                return Ok(());
            }
            (state.record.clone(), source)
        };
        if let Some(source) = source {
            next.recoveries.remove(&source);
            next.automatic.remove(&source);
        }
        if active {
            next.automatic.insert(pane_id.clone(), descriptor.clone());
        } else {
            next.automatic.remove(pane_id);
        }
        next.recoveries.insert(pane_id.clone(), descriptor);
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |state| {
            state.resuming.remove(pane_id);
        })?;
        Ok(())
    }
    pub(super) fn automatic_end(&self, launch: &crate::harness::Launch) -> Result<()> {
        let mut next = {
            let mut state = self.inner.state.lock().unwrap();
            if state.closing {
                return Ok(());
            }
            healthy(&state)?;
            let Some(native) = launch.managed.conversation.as_ref() else {
                return Ok(());
            };
            directories::end_native(
                &mut state,
                &launch.grant.pane_id,
                launch.grant.kind,
                &native.id,
            );
            if !state
                .record
                .automatic
                .get(&launch.grant.pane_id)
                .is_some_and(|saved| {
                    saved.kind == launch.grant.kind && saved.conversation_id == native.id
                })
            {
                return Ok(());
            }
            state.record.clone()
        };
        next.automatic.remove(&launch.grant.pane_id);
        next.registry.revision = next_revision(next.registry.revision)?;
        commit(&self.inner, next, |_| {})?;
        Ok(())
    }
    pub(super) fn revoke_recovery(&self, pane_id: &str) -> Result<()> {
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut next = {
            let mut state = self.inner.state.lock().unwrap();
            // Failed unpublished spawns and daemon shutdown still need native
            // cleanup, but must not consume recoverable intent.
            if state.closing || !state.live.contains_key(pane_id) {
                return Ok(());
            }
            healthy(&state)?;
            ensure!(
                !state
                    .resuming
                    .iter()
                    .any(|(id, attempt)| id != pane_id && attempt.source == pane_id),
                "pane busy: native resume in progress"
            );
            let mut next = state.record.clone();
            next.recoveries.remove(pane_id);
            next.automatic.remove(pane_id);
            if let Some(attempt) = state.resuming.get(pane_id) {
                next.recoveries.remove(&attempt.source);
                next.automatic.remove(&attempt.source);
            }
            if next.recoveries == state.record.recoveries {
                state.closing_panes.insert(pane_id.to_owned());
                state.resuming.remove(pane_id);
                state.failed_restores.remove(pane_id);
                return Ok(());
            }
            next
        };
        next.registry.revision = next_revision(next.registry.revision)?;
        commit(&self.inner, next, |state| {
            state.closing_panes.insert(pane_id.to_owned());
            state.resuming.remove(pane_id);
            state.failed_restores.remove(pane_id);
        })?;
        Ok(())
    }
}
