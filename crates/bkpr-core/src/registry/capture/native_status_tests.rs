use super::*;
use crate::harness::NativeStatusStream;

struct StatusFixture {
    fixture: Fixture,
    pane_id: String,
    grant: CaptureGrant,
    peer: Peer,
}
impl StatusFixture {
    fn new() -> Self {
        let fixture = Fixture::new();
        let (pane_id, grant, peer) =
            seed_launch_command(&fixture, HarnessKind::Opencode, "IFS= read -r hold");
        let this = Self {
            fixture,
            pane_id,
            grant,
            peer,
        };
        this.bind("session-one");
        this
    }
    fn event(&self, event: CaptureEvent) -> Request {
        Request::new(Operation::HarnessEvent {
            launch_id: self.grant.launch_id.clone(),
            token: self.grant.token.clone(),
            event,
        })
    }
    fn bind(&self, id: &str) {
        let original = {
            let mut captures = self.fixture.registry.inner.captures.lock().unwrap();
            let original = captures
                .lookup(&self.grant.launch_id, &self.grant.token)
                .unwrap();
            captures
                .event(
                    &self.grant.launch_id,
                    &self.grant.token,
                    CaptureEvent::Bind {
                        conversation_id: id.into(),
                        source: ConversationSource::OpencodePlugin,
                        expected_revision: Some(original.binding_generation),
                    },
                )
                .unwrap();
            original
        };
        self.fixture
            .registry
            .publish_capture_change(&original)
            .unwrap();
    }
    fn enroll(&self) -> NativeStatusStream {
        let id = self
            .fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches[&self.grant.launch_id]
            .managed
            .conversation
            .as_ref()
            .unwrap()
            .id
            .clone();
        let (stream, response) = self
            .fixture
            .registry
            .register_native_status(
                &self.event(CaptureEvent::StatusEnroll {
                    conversation_id: id,
                    source: ConversationSource::OpencodePlugin,
                }),
                &self.peer,
                &|| true,
            )
            .unwrap();
        assert_eq!(response["generation"], stream.binding.generation);
        stream
    }
    fn report_request(&self, stream: &NativeStatusStream, status: NativeStatus) -> Request {
        self.event(CaptureEvent::StatusReport {
            conversation_id: stream.binding.id.clone(),
            source: stream.binding.source,
            generation: stream.binding.generation,
            status,
        })
    }
    fn report(&self, stream: &NativeStatusStream, state: NativeStatusState) -> Result<Value> {
        self.fixture.registry.report_native_status(
            stream,
            self.report_request(stream, NativeStatus { state, kind: None }),
            &self.peer,
            &|| true,
        )
    }
    fn info(&self) -> PaneInfo {
        self.fixture
            .registry
            .snapshot()
            .unwrap()
            .panes
            .into_iter()
            .find(|pane| pane.id == self.pane_id)
            .unwrap()
    }
}

#[test]
fn enrollment_and_reports_reject_copied_foreign_and_mismatched_authority() {
    let fixture = StatusFixture::new();
    for (source, id) in [
        (ConversationSource::ClaudeHook, "session-one"),
        (ConversationSource::OpencodePlugin, "foreign-session"),
    ] {
        assert!(
            fixture
                .fixture
                .registry
                .register_native_status(
                    &fixture.event(CaptureEvent::StatusEnroll {
                        conversation_id: id.into(),
                        source,
                    }),
                    &fixture.peer,
                    &|| true
                )
                .is_err()
        );
    }
    let process = proc::process(std::process::id() as i32).unwrap().unwrap();
    let foreign = Peer {
        pid: process.pid,
        uid: unsafe { libc::geteuid() },
        birth: process.identity,
    };
    assert!(
        fixture
            .fixture
            .registry
            .register_native_status(
                &fixture.event(CaptureEvent::StatusEnroll {
                    conversation_id: "session-one".into(),
                    source: ConversationSource::OpencodePlugin,
                }),
                &foreign,
                &|| true
            )
            .is_err(),
        "a copied token cannot enroll an unrelated process"
    );
    let stream = fixture.enroll();
    let status = NativeStatus {
        state: NativeStatusState::Working,
        kind: None,
    };
    for peer in [
        foreign,
        Peer {
            pid: fixture.peer.pid,
            uid: fixture.peer.uid.wrapping_add(1),
            birth: fixture.peer.birth.clone(),
        },
        Peer {
            pid: fixture.peer.pid,
            uid: fixture.peer.uid,
            birth: "reused PID".into(),
        },
    ] {
        assert!(
            fixture
                .fixture
                .registry
                .report_native_status(
                    &stream,
                    fixture.report_request(&stream, status),
                    &peer,
                    &|| true
                )
                .is_err()
        );
    }
    for change in 0..4 {
        let mut request = fixture.report_request(&stream, status);
        let Operation::HarnessEvent {
            token,
            event:
                CaptureEvent::StatusReport {
                    source,
                    conversation_id,
                    generation,
                    ..
                },
            ..
        } = &mut request.operation
        else {
            unreachable!()
        };
        match change {
            0 => *source = ConversationSource::ClaudeHook,
            1 => *conversation_id = "foreign-session".into(),
            2 => *generation += 1,
            _ => *token = "copied-invalid-token".into(),
        }
        assert!(
            fixture
                .fixture
                .registry
                .report_native_status(&stream, request, &fixture.peer, &|| true)
                .is_err()
        );
    }
    assert!(
        fixture
            .fixture
            .registry
            .request_authenticated(fixture.report_request(&stream, status), &fixture.peer)
            .is_err(),
        "an ordinary socket cannot borrow an enrolled stream"
    );
    assert!(
        fixture
            .fixture
            .registry
            .report_native_status(
                &stream,
                fixture.report_request(
                    &stream,
                    NativeStatus {
                        state: NativeStatusState::Working,
                        kind: Some(NativeStatusKind::Permission),
                    }
                ),
                &fixture.peer,
                &|| true
            )
            .is_err()
    );
    let accepted = fixture.report(&stream, NativeStatusState::Working).unwrap();
    assert_eq!(
        accepted,
        json!({"accepted":true,"generation":stream.binding.generation})
    );
    let status = fixture.info().harness_status.unwrap();
    assert_eq!(status.app.as_deref(), Some("opencode"));
    assert_eq!(status.id, format!("native:{}", fixture.grant.launch_id));
    assert!(status.message.is_none() && status.progress.is_none() && status.title.is_empty());
}

#[test]
fn binding_switch_back_and_stream_replacement_never_restore_old_authority() {
    let fixture = StatusFixture::new();
    let old = fixture.enroll();
    fixture.report(&old, NativeStatusState::Working).unwrap();
    fixture.bind("session-one");
    assert!(fixture.report(&old, NativeStatusState::Done).is_err());
    assert!(fixture.info().harness_status.is_none());
    let same_root = fixture.enroll();
    assert!(same_root.binding.generation > old.binding.generation);
    fixture.report(&same_root, NativeStatusState::Done).unwrap();
    fixture.bind("session-two");
    assert!(fixture.info().harness_status.is_none());
    fixture.bind("session-one");
    assert!(fixture.report(&old, NativeStatusState::Working).is_err());
    assert!(
        fixture
            .report(&same_root, NativeStatusState::Working)
            .is_err()
    );
    let replacement = fixture.enroll();
    assert!(replacement.binding.generation > old.binding.generation);
    fixture
        .report(&replacement, NativeStatusState::Blocked)
        .unwrap();
    fixture.fixture.registry.end_native_status(&old).unwrap();
    assert_eq!(fixture.info().harness_status.unwrap().state, "blocked");
    let newest = fixture.enroll();
    assert_eq!(newest.binding.generation, replacement.binding.generation);
    assert!(
        fixture
            .report(&replacement, NativeStatusState::Error)
            .is_err()
    );
    fixture.report(&newest, NativeStatusState::Idle).unwrap();
    fixture
        .fixture
        .registry
        .end_native_status(&replacement)
        .unwrap();
    assert_eq!(fixture.info().harness_status.unwrap().state, "idle");
    fixture.fixture.registry.end_native_status(&newest).unwrap();
    assert!(fixture.info().harness_status.is_none());
    assert!(
        fixture.info().harness.unwrap().managed.is_some(),
        "status EOF must not retire the launch"
    );
}

#[test]
fn native_precedence_timestamps_background_hiding_and_eof_preserve_independent_osc() {
    let fixture = StatusFixture::new();
    let pane = fixture
        .fixture
        .registry
        .capture_pane(&fixture.pane_id)
        .unwrap();
    let durable = fs::read(fixture.fixture._root.path().join("registry.json")).unwrap();
    {
        let mut published = pane.shared.published.lock().unwrap();
        published.status.push(ProgramStatus {
            state: "error".into(),
            kind: None,
            app: Some("opencode".into()),
            id: "osc".into(),
            message: None,
            title: String::new(),
            progress: None,
        });
        published.status_changed_at_ms.insert("osc".into(), 11);
    }
    pane.shared.status_dirty.store(true, Ordering::Release);
    fixture.fixture.registry.refresh_status_summaries();
    assert_eq!(fixture.info().harness_status_changed_at_ms, Some(11));
    let stream = fixture.enroll();
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    assert_eq!(
        fixture.info().harness_status.unwrap().state,
        "working",
        "native overrides even a higher-priority OSC error"
    );
    let changed = fixture.info().harness_status_changed_at_ms;
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    assert_eq!(fixture.info().harness_status_changed_at_ms, changed);
    {
        let mut published = pane.shared.published.lock().unwrap();
        published.status[0].state = "blocked".into();
        published.status_changed_at_ms.insert("osc".into(), 22);
    }
    pane.shared.status_dirty.store(true, Ordering::Release);
    fixture.fixture.registry.refresh_status_summaries();
    assert_eq!(fixture.info().harness_status.unwrap().state, "working");
    assert_eq!(fixture.info().harness_status_changed_at_ms, changed);
    fixture
        .fixture
        .registry
        .inner
        .captures
        .lock()
        .unwrap()
        .native_status
        .get_mut(&fixture.grant.launch_id)
        .unwrap()
        .observation
        .as_mut()
        .unwrap()
        .changed_at_ms = 1;
    for kind in [
        NativeStatusKind::Permission,
        NativeStatusKind::Question,
        NativeStatusKind::Auth,
    ] {
        let request = fixture.report_request(
            &stream,
            NativeStatus {
                state: NativeStatusState::Blocked,
                kind: Some(kind),
            },
        );
        fixture
            .fixture
            .registry
            .report_native_status(&stream, request, &fixture.peer, &|| true)
            .unwrap();
        assert_eq!(
            fixture.info().harness_status.unwrap().kind.as_deref(),
            Some(kind.as_str())
        );
        assert!(fixture.info().harness_status_changed_at_ms.unwrap() > 1);
    }
    fixture.report(&stream, NativeStatusState::Clear).unwrap();
    assert_eq!(fixture.info().harness_status.unwrap().state, "blocked");
    assert_eq!(fixture.info().harness_status_changed_at_ms, Some(22));
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    fixture.fixture.registry.end_native_status(&stream).unwrap();
    assert_eq!(fixture.info().harness_status.unwrap().id, "osc");
    assert_eq!(fixture.info().harness_status_changed_at_ms, Some(22));
    let stream = fixture.enroll();
    let foreground = fixture.info().harness.unwrap();
    fixture
        .fixture
        .registry
        .publish_facts(&fixture.pane_id, &pane, None, None, None)
        .unwrap();
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    assert!(fixture.info().harness_status.is_none());
    assert!(fixture.info().harness_status_changed_at_ms.is_none());
    fixture
        .fixture
        .registry
        .publish_facts(
            &fixture.pane_id,
            &pane,
            Some(fixture.fixture.home.clone()),
            Some(foreground),
            None,
        )
        .unwrap();
    assert_eq!(fixture.info().harness_status.unwrap().state, "working");
    assert!(pane.shared.authority.lock().unwrap().views.is_empty());
    assert_eq!(
        fs::read(fixture.fixture._root.path().join("registry.json")).unwrap(),
        durable
    );
}

#[test]
fn retirement_replacement_and_failed_publication_never_acknowledge_status() {
    let fixture = StatusFixture::new();
    let stream = fixture.enroll();
    let previous_revision = fixture
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .registry
        .revision;
    fixture
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .registry
        .revision = u64::MAX;
    assert!(fixture.report(&stream, NativeStatusState::Working).is_err());
    assert!(fixture.info().harness_status.is_none());
    assert!(
        fixture
            .fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .native_status[&fixture.grant.launch_id]
            .observation
            .is_none()
    );
    fixture
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .registry
        .revision = previous_revision;
    assert!(
        fixture
            .fixture
            .registry
            .report_native_status(
                &stream,
                fixture.report_request(
                    &stream,
                    NativeStatus {
                        state: NativeStatusState::Working,
                        kind: None
                    }
                ),
                &fixture.peer,
                &|| false
            )
            .is_err()
    );
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    let original = fixture
        .fixture
        .registry
        .inner
        .captures
        .lock()
        .unwrap()
        .lookup(&fixture.grant.launch_id, &fixture.grant.token)
        .unwrap();
    fixture
        .fixture
        .registry
        .inner
        .captures
        .lock()
        .unwrap()
        .retire_pane(&fixture.pane_id);
    fixture
        .fixture
        .registry
        .publish_capture_change(&original)
        .unwrap();
    assert!(fixture.report(&stream, NativeStatusState::Done).is_err());
    assert!(fixture.info().harness_status.is_none());
    let replacement = fixture
        .fixture
        .registry
        .inner
        .captures
        .lock()
        .unwrap()
        .insert(
            original.root.clone(),
            original.pane_root.clone(),
            fixture.pane_id.clone(),
            HarnessKind::Opencode,
        )
        .unwrap();
    assert_ne!(replacement.launch_id, fixture.grant.launch_id);
    assert!(fixture.report(&stream, NativeStatusState::Working).is_err());
    fixture.fixture.registry.end_native_status(&stream).unwrap();
    assert!(
        fixture
            .fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .contains_key(&replacement.launch_id)
    );
}

fn notification_fixture() -> (StatusFixture, crate::notifications::NotificationService) {
    let fixture = Fixture::new();
    let service = crate::notifications::NotificationService::open(
        &fixture._root.path().join("notifications"),
    )
    .unwrap();
    fixture
        .registry
        .configure_notifications(service.clone())
        .unwrap();
    service
        .request(
            NotificationRequest::PolicySet {
                policy: NotificationPolicy {
                    scope: NotificationScope::default(),
                    kinds: vec![
                        NotificationKind::Attention,
                        NotificationKind::Error,
                        NotificationKind::Idle,
                        NotificationKind::Done,
                    ],
                },
            },
            None,
            &|| true,
        )
        .unwrap();
    let (pane_id, grant, peer) =
        seed_launch_command(&fixture, HarnessKind::Opencode, "IFS= read -r hold");
    let fixture = StatusFixture {
        fixture,
        pane_id,
        grant,
        peer,
    };
    fixture.bind("session-one");
    (fixture, service)
}

fn notification_page(service: &crate::notifications::NotificationService) -> NotificationPage {
    serde_json::from_value(
        service
            .request(
                NotificationRequest::Inspect {
                    query: NotificationQuery::default(),
                    after: None,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap()
}

fn notification_terminal(
    fixture: &StatusFixture,
) -> (bkpr_terminal_engine::Terminal, Arc<crate::Shared>) {
    let pane = fixture
        .fixture
        .registry
        .capture_pane(&fixture.pane_id)
        .unwrap();
    let shared = pane.shared.clone();
    shared.status_feed_epoch.store(
        shared.status_epoch.load(Ordering::Acquire),
        Ordering::Release,
    );
    let mut terminal = bkpr_terminal_engine::Terminal::new(Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap();
    let callback = shared.clone();
    terminal.set_program_status_observer(Box::new(move |records, explicit| {
        callback.status_observer.get().unwrap()(records, explicit);
    }));
    (terminal, shared)
}

#[test]
fn notification_capture_keeps_bursts_before_frames_and_deduplicates_reports() {
    let (fixture, service) = notification_fixture();
    let (mut terminal, _) = notification_terminal(&fixture);
    terminal.feed(b"\x1b]7501;state=blocked:kind=permission:app=opencode\x07\x1b]7501;state=blocked:kind=permission:app=opencode\x07\x1b]7501;state=working:app=opencode\x07\x1b]7501;state=blocked:kind=question:app=opencode\x07").unwrap();
    assert!(
        fixture.info().harness_status.is_none(),
        "no frame/status summary was published"
    );
    let page = notification_page(&service);
    assert_eq!(page.notifications.len(), 2);
    assert_eq!(
        page.notifications[0].attention_kind.as_deref(),
        Some("permission")
    );
    assert_eq!(
        page.notifications[1].attention_kind.as_deref(),
        Some("question")
    );
    assert!(page.notifications.iter().all(|notification| {
        let source = notification.source.as_pane().unwrap();
        notification.kind == NotificationKind::Attention
            && source.process.pid == fixture.peer.pid as u32
            && source.process.birth == fixture.peer.birth
            && source.launch_id.as_deref() == Some(fixture.grant.launch_id.as_str())
            && source.binding_generation == Some(1)
    }));
    let original = {
        let mut captures = fixture.fixture.registry.inner.captures.lock().unwrap();
        let original = captures
            .lookup(&fixture.grant.launch_id, &fixture.grant.token)
            .unwrap();
        captures
            .launches
            .get_mut(&fixture.grant.launch_id)
            .unwrap()
            .managed
            .first_prompt_submitted_at_ms = Some(42);
        original
    };
    fixture
        .fixture
        .registry
        .publish_capture_change(&original)
        .unwrap();
    terminal
        .feed(b"\x1b]7501;state=blocked:kind=question:app=opencode\x07")
        .unwrap();
    assert_eq!(
        notification_page(&service).notifications.len(),
        2,
        "prompt metadata must not restart an attention episode"
    );
    fixture.fixture.registry.shutdown().unwrap();
    service.shutdown().unwrap();
}

#[test]
fn native_notifications_keep_precedence_and_clear_eof_never_complete() {
    let (fixture, service) = notification_fixture();
    let stream = fixture.enroll();
    fixture.report(&stream, NativeStatusState::Working).unwrap();
    let (mut terminal, _) = notification_terminal(&fixture);
    terminal
        .feed(b"\x1b]7501;state=blocked:app=opencode\x07\x1b]7501;state=done:app=opencode\x07")
        .unwrap();
    assert!(notification_page(&service).notifications.is_empty());
    fixture.report(&stream, NativeStatusState::Blocked).unwrap();
    fixture.report(&stream, NativeStatusState::Blocked).unwrap();
    fixture.report(&stream, NativeStatusState::Clear).unwrap();
    fixture.fixture.registry.end_native_status(&stream).unwrap();
    let page = notification_page(&service);
    assert_eq!(page.notifications.len(), 1);
    assert_eq!(page.notifications[0].kind, NotificationKind::Attention);
    let replacement = fixture.enroll();
    assert!(fixture.report(&stream, NativeStatusState::Done).is_err());
    fixture
        .report(&replacement, NativeStatusState::Working)
        .unwrap();
    fixture.fixture.registry.end_native_status(&stream).unwrap();
    fixture
        .report(&replacement, NativeStatusState::Idle)
        .unwrap();
    fixture
        .report(&replacement, NativeStatusState::Done)
        .unwrap();
    let page = notification_page(&service);
    assert_eq!(
        page.notifications
            .iter()
            .map(|record| record.kind)
            .collect::<Vec<_>>(),
        [
            NotificationKind::Attention,
            NotificationKind::Idle,
            NotificationKind::Done
        ]
    );
    fixture.fixture.registry.shutdown().unwrap();
    service.shutdown().unwrap();
}

#[test]
fn notification_capture_rejects_stale_birth_binding_and_mid_feed_authority() {
    let (fixture, service) = notification_fixture();
    let (mut terminal, shared) = notification_terminal(&fixture);
    shared
        .published
        .lock()
        .unwrap()
        .program_identity
        .as_mut()
        .unwrap()
        .birth = "reused-pid".into();
    terminal
        .feed(b"\x1b]7501;state=blocked:app=opencode\x07")
        .unwrap();
    assert!(notification_page(&service).notifications.is_empty());
    shared
        .published
        .lock()
        .unwrap()
        .program_identity
        .as_mut()
        .unwrap()
        .birth = fixture.peer.birth.clone();
    let previous = fixture.info().harness.unwrap().managed;
    fixture.bind("session-two");
    let current = fixture.info().harness.unwrap().managed;
    {
        let mut state = fixture.fixture.registry.inner.state.lock().unwrap();
        state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|info| info.id == fixture.pane_id)
            .unwrap()
            .harness
            .as_mut()
            .unwrap()
            .managed = previous;
    }
    shared.status_feed_epoch.store(
        shared.status_epoch.load(Ordering::Acquire),
        Ordering::Release,
    );
    terminal
        .feed(b"\x1b]7501;state=blocked:app=opencode\x07")
        .unwrap();
    assert!(notification_page(&service).notifications.is_empty());
    {
        let mut state = fixture.fixture.registry.inner.state.lock().unwrap();
        state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|info| info.id == fixture.pane_id)
            .unwrap()
            .harness
            .as_mut()
            .unwrap()
            .managed = current;
    }
    let callback = shared.clone();
    let mut first = true;
    terminal.set_program_status_observer(Box::new(move |records, explicit| {
        callback.status_observer.get().unwrap()(records, explicit);
        if first {
            first = false;
            callback.status_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }));
    terminal.feed(b"\x1b]7501;state=blocked:app=opencode\x07\x1b]7501;state=working:app=opencode\x07\x1b]7501;state=blocked:app=opencode\x07").unwrap();
    let page = notification_page(&service);
    assert_eq!(
        page.notifications.len(),
        1,
        "retired feed cannot acquire the replacement authority"
    );
    assert_eq!(
        page.notifications[0]
            .source
            .as_pane()
            .unwrap()
            .binding_generation,
        Some(2)
    );
    shared.status_feed_epoch.store(
        shared.status_epoch.load(Ordering::Acquire),
        Ordering::Release,
    );
    terminal
        .feed(b"\x1b]7501;state=blocked:app=opencode\x07")
        .unwrap();
    assert_eq!(
        notification_page(&service).notifications.len(),
        1,
        "a rejected old feed cannot withdraw and reopen the live attention episode"
    );
    fixture.bind("session-three");
    let (mut terminal, _) = notification_terminal(&fixture);
    terminal
        .feed(b"\x1b]7501;state=blocked:app=opencode\x07")
        .unwrap();
    let page = notification_page(&service);
    assert_eq!(page.notifications.len(), 2);
    assert_eq!(
        page.notifications[1]
            .source
            .as_pane()
            .unwrap()
            .binding_generation,
        Some(3)
    );
    fixture.fixture.registry.shutdown().unwrap();
    service.shutdown().unwrap();
}
