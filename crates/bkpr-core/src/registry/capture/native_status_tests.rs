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
