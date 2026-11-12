use super::*;
use crate::harness::{NativeStatusChannel, NativeStatusObservation, NativeStatusStream};

impl Registry {
    /// Enroll an authenticated process for the exact current native binding.
    pub(crate) fn register_native_status(
        &self,
        request: &Request,
        peer: &Peer,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<(NativeStatusStream, Value)> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        let Operation::HarnessEvent {
            launch_id,
            token,
            event:
                CaptureEvent::StatusEnroll {
                    conversation_id,
                    source,
                },
        } = &request.operation
        else {
            bail!("native status requires an enrollment request")
        };
        let original = self
            .inner
            .captures
            .lock()
            .unwrap()
            .lookup(launch_id, token)?;
        let chain = harness::validate_callback(peer, &original)?;
        let launch_lease = original.control_lease()?;
        let origin_lease = crate::ControlLease::default();
        origin_lease.bind(&chain[0], &original.pane_root)?;
        let pane = self.capture_pane(&original.grant.pane_id)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(
            !state.closing && !pane.shared.finished.load(Ordering::Acquire),
            "native status pane ended"
        );
        ensure!(
            state
                .live
                .get(&original.grant.pane_id)
                .is_some_and(|current| { Arc::ptr_eq(&current.shared, &pane.shared) }),
            "native status pane replaced"
        );
        let mut captures = self.inner.captures.lock().unwrap();
        let launch = captures.lookup(launch_id, token)?;
        let binding = current_binding(&launch, conversation_id, *source)?.clone();
        ensure!(
            launch.managed.conversation == original.managed.conversation,
            "native status binding changed during enrollment"
        );
        launch_lease.ensure_active()?;
        origin_lease.ensure_active()?;
        ensure!(is_connected(), "native status caller disconnected");
        let revision = next_revision(state.record.registry.revision)?;
        let info = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|info| info.id == launch.grant.pane_id && info.status == PaneStatus::Running)
            .context("native status pane is not running")?;
        let published = pane.shared.published.lock().unwrap();
        ensure!(
            published.lifecycle == "running",
            "native status pane is not running"
        );
        captures.clear_native_status(launch_id);
        captures.native_status.insert(
            launch_id.clone(),
            NativeStatusChannel {
                binding: binding.clone(),
                peer: ProcessIdentity {
                    pid: peer.pid as u32,
                    birth: peer.birth.clone(),
                },
                origin: origin_lease.clone(),
                launch: launch_lease.clone(),
                observation: None,
            },
        );
        let (summary, changed_at_ms) = visible_status(info.harness.as_ref(), &published, &captures);
        if info.harness_status.as_ref() != summary
            || info.harness_status_changed_at_ms != changed_at_ms
        {
            info.harness_status = summary.cloned();
            info.harness_status_changed_at_ms = changed_at_ms;
            state.record.registry.revision = revision;
            self.inner.changed.notify_all();
        }
        let response = json!({"generation":binding.generation});
        Ok((
            NativeStatusStream {
                grant: launch.grant,
                binding,
                origin: origin_lease,
                launch: launch_lease,
            },
            response,
        ))
    }

    /// Publish a volatile report from the retained enrollment's originating process.
    pub(crate) fn report_native_status(
        &self,
        stream: &NativeStatusStream,
        request: Request,
        peer: &Peer,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        let Operation::HarnessEvent {
            launch_id,
            token,
            event:
                CaptureEvent::StatusReport {
                    conversation_id,
                    source,
                    generation,
                    status,
                },
        } = request.operation
        else {
            bail!("native status stream accepts status reports only")
        };
        status.validate()?;
        ensure!(
            launch_id == stream.grant.launch_id
                && harness::constant_equal(token.as_bytes(), stream.grant.token.as_bytes())
                && conversation_id == stream.binding.id
                && source == stream.binding.source
                && generation == stream.binding.generation,
            "native status report does not match enrollment"
        );
        stream.ensure_active()?;
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(!state.closing, "native status daemon is closing");
        let mut captures = self.inner.captures.lock().unwrap();
        let launch = captures
            .launches
            .get(&launch_id)
            .context("unknown or ended launch")?;
        ensure!(
            harness::constant_equal(token.as_bytes(), launch.grant.token.as_bytes()),
            "invalid capture capability"
        );
        let binding = current_binding(launch, &conversation_id, source)?;
        ensure!(
            binding.generation == generation,
            "native status generation changed"
        );
        let channel = captures
            .native_status
            .get(&launch_id)
            .context("native status is not enrolled")?;
        ensure!(
            channel.origin.same_authority(&stream.origin),
            "native status enrollment was replaced"
        );
        channel.ensure_reporter(peer, binding)?;
        let pane_id = &launch.grant.pane_id;
        ensure!(
            !state.closing_panes.contains(pane_id)
                && !state.closing_document_panes.contains(pane_id),
            "native status pane is closing"
        );
        let pane = state
            .live
            .get(pane_id)
            .context("native status pane unavailable")?
            .clone();
        ensure!(
            !pane.shared.finished.load(Ordering::Acquire),
            "native status pane ended"
        );
        ensure!(
            pane.shared
                .root
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|root| {
                    root.pid == launch.pane_root.pid && root.identity == launch.pane_root.identity
                }),
            "native status pane root was replaced"
        );
        let published = pane.shared.published.lock().unwrap();
        ensure!(
            published.lifecycle == "running",
            "native status pane is not running"
        );
        ensure!(is_connected(), "native status caller disconnected");
        let revision = next_revision(state.record.registry.revision)?;
        let app = launch.grant.kind.as_str();
        let info = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|info| &info.id == pane_id && info.status == PaneStatus::Running)
            .context("native status pane is not running")?;
        let channel = captures.native_status.get_mut(&launch_id).unwrap();
        if status.state == NativeStatusState::Clear {
            channel.observation = None;
        } else if channel.observation.as_ref().is_none_or(|observation| {
            observation.status.state != status.state.as_str()
                || observation.status.kind.as_deref() != status.kind.map(NativeStatusKind::as_str)
        }) {
            channel.observation = Some(NativeStatusObservation {
                status: ProgramStatus {
                    state: status.state.as_str().into(),
                    kind: status.kind.map(|kind| kind.as_str().into()),
                    app: Some(app.into()),
                    id: format!("native:{launch_id}"),
                    message: None,
                    title: String::new(),
                    progress: None,
                },
                changed_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            });
        }
        let (summary, changed_at_ms) = visible_status(info.harness.as_ref(), &published, &captures);
        if info.harness_status.as_ref() != summary
            || info.harness_status_changed_at_ms != changed_at_ms
        {
            info.harness_status = summary.cloned();
            info.harness_status_changed_at_ms = changed_at_ms;
            state.record.registry.revision = revision;
            self.inner.changed.notify_all();
        }
        Ok(json!({"accepted":true,"generation":generation}))
    }

    /// Remove only the observation owned by this enrollment.
    pub(crate) fn end_native_status(&self, stream: &NativeStatusStream) -> Result<()> {
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut state = self.inner.state.lock().unwrap();
        let mut captures = self.inner.captures.lock().unwrap();
        if !captures
            .native_status
            .get(&stream.grant.launch_id)
            .is_some_and(|channel| {
                channel.binding == stream.binding && channel.origin.same_authority(&stream.origin)
            })
        {
            return Ok(());
        }
        captures.clear_native_status(&stream.grant.launch_id);
        let Some(pane) = state.live.get(&stream.grant.pane_id).cloned() else {
            return Ok(());
        };
        let revision = next_revision(state.record.registry.revision)?;
        let Some(info) = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|info| info.id == stream.grant.pane_id && info.status == PaneStatus::Running)
        else {
            return Ok(());
        };
        let published = pane.shared.published.lock().unwrap();
        let (summary, changed_at_ms) = visible_status(info.harness.as_ref(), &published, &captures);
        if info.harness_status.as_ref() != summary
            || info.harness_status_changed_at_ms != changed_at_ms
        {
            info.harness_status = summary.cloned();
            info.harness_status_changed_at_ms = changed_at_ms;
            state.record.registry.revision = revision;
            self.inner.changed.notify_all();
        }
        Ok(())
    }
}

fn current_binding<'a>(
    launch: &'a Launch,
    conversation_id: &str,
    source: ConversationSource,
) -> Result<&'a NativeConversation> {
    let binding = launch
        .managed
        .conversation
        .as_ref()
        .context("native status requires a bound conversation")?;
    ensure!(
        launch.grant.kind == source.harness()
            && binding.id == conversation_id
            && binding.source == source,
        "native status does not match the bound conversation"
    );
    Ok(binding)
}
