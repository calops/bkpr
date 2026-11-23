//! Explicit native resume intent. This store never grants live capture or inbox authority.
use super::*;
use crate::control_lease::ProcessLifetime;
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SavedParticipant {
    pub(super) workspace_id: String,
    pub(super) id: channels::ParticipantId,
}

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
fn resume_flag(kind: HarnessKind) -> Result<&'static str> {
    Ok(match kind {
        HarnessKind::Claude | HarnessKind::Omp => "--resume",
        HarnessKind::Codex => "resume",
        HarnessKind::Agy => "--conversation",
        HarnessKind::Pi => "--session",
        HarnessKind::Opencode => "--bkpr-resume",
        _ => bail!("harness does not support managed recovery"),
    })
}
fn same_conversation(a: &HarnessRecovery, b: &HarnessRecovery) -> bool {
    a.kind == b.kind && a.conversation_id == b.conversation_id
}

struct TitleSource {
    launch_id: String,
    generation: u64,
    group: i32,
    lifetime: ProcessLifetime,
}

/// Pane-local title observation and its non-durable process witness.
#[derive(Default)]
pub(crate) struct TitleCapture {
    source: Option<TitleSource>,
    observed: Option<String>,
}

impl TitleCapture {
    pub(super) fn end(&mut self) {
        self.source = None;
    }

    /// Whether there is a managed title witness to consult on OSC title effects.
    pub(crate) fn has_source(&self) -> bool {
        self.source.is_some()
    }
}

impl Record {
    pub(super) fn remove_recovery(&mut self, id: &str) {
        self.recoveries.remove(id);
        self.automatic.remove(id);
        self.recovery_titles.remove(id);
        self.channel_participants.remove(id);
    }
}

pub(super) fn pending_title(state: &State, id: &str) -> Option<String> {
    let descriptor = state.record.recoveries.get(id)?;
    if !state.record.automatic_titles.contains(id) {
        return None;
    }
    let pane = state.live.get(id)?;
    let published = pane.shared.published.lock().unwrap();
    let title = published.recovery_title.observed.as_ref()?;
    (!same_display_title(descriptor.kind, state.record.recovery_titles.get(id), title))
        .then(|| title.clone())
}

pub(super) fn save_title(state: &State, next: &mut Record, id: &str) {
    if let Some(title) = pending_title(state, id) {
        next.recovery_titles.insert(id.to_owned(), title);
    }
}

fn same_display_title(kind: HarnessKind, previous: Option<&String>, title: &str) -> bool {
    previous.is_some_and(|old| kind.display_title(old) == kind.display_title(title))
}

fn capture_title(
    inner: &Inner,
    shared: &crate::Shared,
    title: &str,
    epoch: u64,
    foreground_group: i32,
) {
    if valid_name(title).is_err() {
        return;
    }
    let id = &shared.pane_id;
    let state = inner.state.lock().unwrap();
    if state.closing
        || state.durability_error.is_some()
        || !state.record.automatic_titles.contains(id)
        || !state
            .live
            .get(id)
            .is_some_and(|pane| std::ptr::eq(pane.shared.as_ref(), shared))
        || shared.finished.load(std::sync::atomic::Ordering::Acquire)
        || shared
            .status_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            != epoch
    {
        return;
    }
    let Some(descriptor) = state.record.recoveries.get(id) else {
        return;
    };
    let Some(harness) = state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| &pane.id == id && pane.status == PaneStatus::Running)
        .and_then(|pane| pane.harness.as_ref())
    else {
        return;
    };
    let Some(managed) = &harness.managed else {
        return;
    };
    let Some(binding) = &managed.conversation else {
        return;
    };
    if descriptor.kind != harness.kind
        || descriptor.conversation_id != binding.id
        || harness.kind.display_title(title).trim().is_empty()
    {
        return;
    }
    let captures = inner.captures.lock().unwrap();
    let Some(launch) = captures.launches.get(&managed.launch_id) else {
        return;
    };
    if &launch.grant.pane_id != id
        || launch.grant.kind != harness.kind
        || launch.binding_generation != binding.generation
        || launch.managed.conversation.as_ref() != Some(binding)
        || !launch.control.is_valid()
    {
        return;
    }
    let mut published = shared.published.lock().unwrap();
    if published.lifecycle != "running"
        || !published
            .program_identity
            .as_ref()
            .is_some_and(|process| process.pid == harness.pid && process.birth == harness.birth)
    {
        return;
    }
    let capture = &mut published.recovery_title;
    let Some(source) = &capture.source else {
        return;
    };
    // OSC has no sender identity. Require the bound launch's verified process
    // lifetime and foreground job, not just the last observed title of the PTY.
    if source.launch_id != managed.launch_id
        || source.generation != binding.generation
        || source.group != foreground_group
        || !source.lifetime.is_alive()
    {
        return;
    }
    let previous = capture
        .observed
        .as_ref()
        .or_else(|| state.record.recovery_titles.get(id));
    if !same_display_title(harness.kind, previous, title) {
        capture.observed = Some(title.to_owned());
    }
}

fn publish_title_source(state: &State, id: &str, replace: bool, source: Option<TitleSource>) {
    if let Some(pane) = state.live.get(id) {
        let mut published = pane.shared.published.lock().unwrap();
        if replace {
            published.recovery_title.observed = None;
        }
        published.recovery_title.source = source;
    }
}

fn candidate_title(state: &State, pane: &PaneInfo, kind: HarnessKind) -> String {
    if !state.record.automatic_titles.contains(&pane.id) {
        return pane.title.clone();
    }
    if let Some(live) = state.live.get(&pane.id)
        && let Some(title) = &live
            .shared
            .published
            .lock()
            .unwrap()
            .recovery_title
            .observed
    {
        return title.clone();
    }
    state
        .record
        .recovery_titles
        .get(&pane.id)
        .cloned()
        .unwrap_or_else(|| kind.display_name().to_owned())
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
    pub(super) fn install_recovery_title_capture(&self, pane: &Pane) {
        let weak = Arc::downgrade(&self.inner);
        let _ = pane.shared.recovery_title_observer.set(Box::new(
            move |shared, title, epoch, group| {
                if let Some(inner) = weak.upgrade() {
                    capture_title(&inner, shared, title, epoch, group);
                }
            },
        ));
    }

    pub(super) fn recovery_candidates(&self) -> Result<Vec<RecoveryCandidate>> {
        let configured = self.inner.spawn_environment.lock().unwrap().is_some();
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        let captures = self.inner.captures.lock().unwrap();
        let panes: BTreeMap<_, _> = state
            .record
            .registry
            .panes
            .iter()
            .map(|pane| (pane.id.as_str(), pane))
            .collect();
        Ok(state
            .record
            .recoveries
            .iter()
            .map(|(id, descriptor)| {
                let pane = panes
                    .get(id.as_str())
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
                        resume_flag(descriptor.kind)?;
                        ensure!(
                            pane.status != PaneStatus::Running
                                && state.live.get(id).is_none_or(|pane| pane
                                    .shared
                                    .finished
                                    .load(std::sync::atomic::Ordering::Acquire)),
                            "recovery pane execution is still running"
                        );
                        if let Some(workspace_id) = &pane.workspace_id {
                            workspace_available(&state, workspace_id)?;
                        } else {
                            ensure!(
                                state.closing_workspaces.is_empty(),
                                "workspace close in progress"
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
                    title: candidate_title(&state, pane, descriptor.kind),
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
                        let new = SpawnLayoutNode::New {
                            reference: "pane".into(),
                        };
                        let layout = match &window.layout {
                            None => new,
                            Some(first) => SpawnLayoutNode::Split {
                                axis: Axis::Horizontal,
                                ratio: 500,
                                first: Box::new(existing_spawn_layout(first)),
                                second: Box::new(new),
                            },
                        };
                        SpawnTarget::Place {
                            workspace_id: Some(workspace_id.clone()),
                            window_id: Some(window_id.clone()),
                            expected_revision: Some(window.revision),
                            layout: Some(layout),
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
        let flag = resume_flag(expected.kind)?;
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
        next.remove_recovery(pane_id);
        next.registry.revision = next_revision(next.registry.revision)?;
        commit(&self.inner, next, |state| {
            publish_title_source(state, pane_id, true, None);
        })?;
        Ok(())
    }
    pub(super) fn recovery_bind(
        &self,
        launch: &crate::harness::Launch,
        descriptor: HarnessRecovery,
        active: bool,
        title_lifetime: Option<ProcessLifetime>,
    ) -> Result<()> {
        validate(&descriptor)?;
        let pane_id = &launch.grant.pane_id;
        let (mut next, source, replaced, title_source, participant, generation) = {
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            // This authenticated binding supersedes earlier sampled role/CWD,
            // including when its durable descriptor is otherwise unchanged.
            state.pending_directories.remove(pane_id);
            let captures = self.inner.captures.lock().unwrap();
            let current = captures
                .launches
                .get(&launch.grant.launch_id)
                .context("native binding launch ended")?;
            let generation = current.binding_generation;
            let title_source = title_lifetime.map(|lifetime| TitleSource {
                launch_id: launch.grant.launch_id.clone(),
                generation,
                group: launch.root.group,
                lifetime,
            });
            let replaced = state
                .record
                .recoveries
                .get(pane_id)
                .is_none_or(|old| !same_conversation(old, &descriptor));
            let attempt = state.resuming.get(pane_id);
            let source = attempt.map(|attempt| attempt.source.clone());
            let workspace = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| &pane.id == pane_id)
                .and_then(|pane| pane.workspace_id.as_deref());
            let participant = workspace.map(|workspace| {
                // Only the backend's fixed resume execution can carry the saved identity.
                let saved = attempt
                    .filter(|attempt| {
                        current.root.pid == current.pane_root.pid
                            && current.root.identity == current.pane_root.identity
                            && same_conversation(&attempt.expected, &descriptor)
                            && state.record.recoveries.get(&attempt.source)
                                == Some(&attempt.expected)
                    })
                    .and_then(|attempt| state.record.channel_participants.get(&attempt.source))
                    .filter(|saved| saved.workspace_id == workspace);
                SavedParticipant {
                    workspace_id: workspace.to_owned(),
                    id: current
                        .channel_participant
                        .as_ref()
                        .or(saved.map(|saved| &saved.id))
                        .cloned()
                        .unwrap_or_else(|| channels::ParticipantId(Uuid::new_v4().to_string())),
                }
            });
            if state.record.recoveries.get(pane_id) == Some(&descriptor)
                && source.is_none()
                && state.record.automatic.get(pane_id) == active.then_some(&descriptor)
                && state.record.channel_participants.get(pane_id) == participant.as_ref()
            {
                publish_title_source(&state, pane_id, false, title_source);
                return Ok(());
            }
            (
                state.record.clone(),
                source,
                replaced,
                title_source,
                participant,
                generation,
            )
        };
        let inherited = source
            .as_ref()
            .and_then(|source| next.recovery_titles.remove(source));
        if let Some(source) = source {
            next.remove_recovery(&source);
        }
        if replaced {
            next.recovery_titles.remove(pane_id);
        }
        if let Some(title) = inherited
            && next.automatic_titles.contains(pane_id)
        {
            next.recovery_titles.insert(pane_id.clone(), title);
        }
        if active {
            next.automatic.insert(pane_id.clone(), descriptor.clone());
        } else {
            next.automatic.remove(pane_id);
        }
        next.recoveries.insert(pane_id.clone(), descriptor);
        if let Some(saved) = &participant {
            next.channel_participants
                .insert(pane_id.clone(), saved.clone());
        } else {
            next.channel_participants.remove(pane_id);
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |state| {
            state.resuming.remove(pane_id);
            let mut captures = self.inner.captures.lock().unwrap();
            if let Some(current) = captures.launches.get_mut(&launch.grant.launch_id)
                && current.binding_generation == generation
            {
                current.channel_participant = participant.map(|saved| saved.id);
            }
            drop(captures);
            publish_title_source(state, pane_id, replaced, title_source);
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
            let mut next = state.record.clone();
            save_title(&state, &mut next, &launch.grant.pane_id);
            next
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
            // Daemon shutdown must clean up native processes without consuming
            // recoverable intent. Unpublished spawns never install this callback.
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
            next.remove_recovery(pane_id);
            if let Some(attempt) = state.resuming.get(pane_id) {
                next.remove_recovery(&attempt.source);
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
