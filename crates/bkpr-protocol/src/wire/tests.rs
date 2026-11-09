use super::*;
use crate::*;
use std::io::Cursor;

fn assert_request(operation: Operation) {
    let original = Request::new(operation);
    let mut codec = Codec::new();
    let decoded = decode_request(codec.encode_request(&original).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&original).unwrap(),
        serde_json::to_value(&decoded).unwrap()
    );
}

#[test]
fn ci_refresh_preserves_exact_source_identity() {
    for checked_ref in [None, Some("origin/feature/日本語".into())] {
        let operation = Operation::CiRefresh {
            target: CiRefreshTarget {
                pane_id: "owning-pane".into(),
                cwd: "/project/account-sensitive/subdirectory".into(),
                repository: ForgeRepository {
                    provider: ForgeProvider::Github,
                    host: "github.example.test".into(),
                    path: "owner/repository".into(),
                    web_url: "https://github.example.test/owner/repository".into(),
                },
                head_sha: "remote-tip".into(),
                local_head_sha: "unpublished-local-tip".into(),
                checked_ref,
            },
        };
        assert_request(operation.clone());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let mut raw = operation.clone();
            let Operation::CiRefresh { target } = &mut raw else {
                unreachable!()
            };
            target.cwd = std::ffi::OsString::from_vec(b"/project/nonutf8-\xff".to_vec()).into();
            assert_request(raw);
        }
        let mut codec = Codec::new();
        let encoded = codec
            .encode_request(&Request::new(operation))
            .unwrap()
            .to_vec();
        let (_, operation_field) = root_field(&encoded, 2);
        let operation_table = indirect(&encoded, operation_field);
        // Dropped source identity must never decode as a broader refresh.
        for slot in [0, 1, 3, 4, 5, 6, 7] {
            let mut missing = encoded.clone();
            let (entry, _) = table_field(&missing, operation_table, slot);
            missing[entry..entry + 2].fill(0);
            assert!(decode_request(&missing).is_err());
        }
    }
}

#[test]
fn every_operation_and_optional_presence_roundtrips() {
    assert_request(Operation::SessionInfo);
    assert_request(Operation::ClientRegister {
        session_id: "日本語\0🦀".into(),
        daemon_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::Client {
        authority: ClientAuthority {
            client_id: "日本語\0🦀".into(),
            token: "日本語\0🦀".into(),
        },
        request: Box::new(Operation::SessionInfo),
    });
    assert_request(Operation::ClientActivate {
        detach_others: true,
        agent_socket: None,
    });
    assert_request(Operation::ClientDetach);
    assert_request(Operation::Inspect);
    assert_request(Operation::Context { pane_id: None });
    assert_request(Operation::RecoveryList);
    assert_request(Operation::PaneResume {
        pane_id: "日本語\0🦀".into(),
        expected: HarnessRecovery {
            kind: HarnessKind::Goose,
            conversation_id: "日本語\0🦀".into(),
            cwd: "/tmp/native".into(),
        },
    });
    assert_request(Operation::HarnessBegin {
        kind: HarnessKind::Goose,
    });
    assert_request(Operation::HarnessEvent {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
        event: CaptureEvent::Bind {
            conversation_id: "日本語\0🦀".into(),
            source: ConversationSource::AgyMcp,
        },
    });
    assert_request(Operation::HarnessContext {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
    });
    assert_request(Operation::Agent {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
        request: AgentRequest::Whoami,
    });
    assert_request(Operation::WorkspaceCreate {
        name: None,
        initial: None,
    });
    assert_request(Operation::WorkspaceRename {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: "日本語\0🦀".into(),
    });
    assert_request(Operation::WorkspaceClose {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
    });
    assert_request(Operation::WindowCreate {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: None,
        spec: PaneSpawnSpec {
            argv: vec!["日本語\0🦀".into()],
            cwd: "/tmp/native".into(),
            home: "/tmp/native".into(),
            size: Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            },
            title: None,
            isolated_environment: true,
        },
    });
    assert_request(Operation::WindowRename {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: "日本語\0🦀".into(),
    });
    assert_request(Operation::WindowClose {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
    });
    assert_request(Operation::PaneSpawn {
        workspace_id: None,
        window_id: None,
        expected_revision: None,
        panes: vec![PaneSpawnEntry {
            reference: "日本語\0🦀".into(),
            spec: PaneSpawnSpec {
                argv: vec!["日本語\0🦀".into()],
                cwd: "/tmp/native".into(),
                home: "/tmp/native".into(),
                size: Size {
                    cols: 17,
                    rows: 17,
                    cell_width: 17,
                    cell_height: 17,
                },
                title: None,
                isolated_environment: true,
            },
        }],
        layout: None,
    });
    assert_request(Operation::PaneReopen {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        pane_id: "日本語\0🦀".into(),
        spec: PaneSpawnSpec {
            argv: vec!["日本語\0🦀".into()],
            cwd: "/tmp/native".into(),
            home: "/tmp/native".into(),
            size: Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            },
            title: None,
            isolated_environment: true,
        },
    });
    assert_request(Operation::PaneTerminate {
        pane_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::PaneForget {
        pane_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::PaneRename {
        pane_id: "日本語\0🦀".into(),
        title: "日本語\0🦀".into(),
    });
    assert_request(Operation::LayoutUpdate {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        layout: None,
    });
    assert_request(Operation::Pane {
        pane_id: "日本語\0🦀".into(),
        request: PaneRequest::Inspect,
    });
    assert_request(Operation::Wait {
        after: u64::MAX,
        wait_ms: u64::MAX,
    });
    assert_request(Operation::Shutdown);
    assert_request(Operation::SessionInfo);
    assert_request(Operation::ClientRegister {
        session_id: "日本語\0🦀".into(),
        daemon_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::Client {
        authority: ClientAuthority {
            client_id: "日本語\0🦀".into(),
            token: "日本語\0🦀".into(),
        },
        request: Box::new(Operation::SessionInfo),
    });
    assert_request(Operation::ClientActivate {
        detach_others: true,
        agent_socket: Some("/tmp/native".into()),
    });
    assert_request(Operation::ClientDetach);
    assert_request(Operation::Inspect);
    assert_request(Operation::Context {
        pane_id: Some("日本語\0🦀".into()),
    });
    assert_request(Operation::RecoveryList);
    assert_request(Operation::PaneResume {
        pane_id: "日本語\0🦀".into(),
        expected: HarnessRecovery {
            kind: HarnessKind::Goose,
            conversation_id: "日本語\0🦀".into(),
            cwd: "/tmp/native".into(),
        },
    });
    assert_request(Operation::HarnessBegin {
        kind: HarnessKind::Goose,
    });
    assert_request(Operation::HarnessEvent {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
        event: CaptureEvent::Bind {
            conversation_id: "日本語\0🦀".into(),
            source: ConversationSource::AgyMcp,
        },
    });
    assert_request(Operation::HarnessContext {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
    });
    assert_request(Operation::Agent {
        launch_id: "日本語\0🦀".into(),
        token: "日本語\0🦀".into(),
        request: AgentRequest::Whoami,
    });
    assert_request(Operation::WorkspaceCreate {
        name: Some("日本語\0🦀".into()),
        initial: Some(PaneSpawnSpec {
            argv: vec!["日本語\0🦀".into()],
            cwd: "/tmp/native".into(),
            home: "/tmp/native".into(),
            size: Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            },
            title: None,
            isolated_environment: true,
        }),
    });
    assert_request(Operation::WorkspaceRename {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: "日本語\0🦀".into(),
    });
    assert_request(Operation::WorkspaceClose {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
    });
    assert_request(Operation::WindowCreate {
        workspace_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: Some("日本語\0🦀".into()),
        spec: PaneSpawnSpec {
            argv: vec!["日本語\0🦀".into()],
            cwd: "/tmp/native".into(),
            home: "/tmp/native".into(),
            size: Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            },
            title: Some("日本語\0🦀".into()),
            isolated_environment: true,
        },
    });
    assert_request(Operation::WindowRename {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        name: "日本語\0🦀".into(),
    });
    assert_request(Operation::WindowClose {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
    });
    assert_request(Operation::PaneSpawn {
        workspace_id: Some("日本語\0🦀".into()),
        window_id: Some("日本語\0🦀".into()),
        expected_revision: Some(u64::MAX),
        panes: vec![PaneSpawnEntry {
            reference: "日本語\0🦀".into(),
            spec: PaneSpawnSpec {
                argv: vec!["日本語\0🦀".into()],
                cwd: "/tmp/native".into(),
                home: "/tmp/native".into(),
                size: Size {
                    cols: 17,
                    rows: 17,
                    cell_width: 17,
                    cell_height: 17,
                },
                title: Some("日本語\0🦀".into()),
                isolated_environment: true,
            },
        }],
        layout: Some(SpawnLayoutNode::New {
            reference: "日本語\0🦀".into(),
        }),
    });
    assert_request(Operation::PaneReopen {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        pane_id: "日本語\0🦀".into(),
        spec: PaneSpawnSpec {
            argv: vec!["日本語\0🦀".into()],
            cwd: "/tmp/native".into(),
            home: "/tmp/native".into(),
            size: Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            },
            title: Some("日本語\0🦀".into()),
            isolated_environment: true,
        },
    });
    assert_request(Operation::PaneTerminate {
        pane_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::PaneForget {
        pane_id: "日本語\0🦀".into(),
    });
    assert_request(Operation::PaneRename {
        pane_id: "日本語\0🦀".into(),
        title: "日本語\0🦀".into(),
    });
    assert_request(Operation::LayoutUpdate {
        workspace_id: "日本語\0🦀".into(),
        window_id: "日本語\0🦀".into(),
        expected_revision: u64::MAX,
        layout: Some(LayoutNode::Leaf {
            pane_id: "日本語\0🦀".into(),
        }),
    });
    assert_request(Operation::Pane {
        pane_id: "日本語\0🦀".into(),
        request: PaneRequest::Inspect,
    });
    assert_request(Operation::Wait {
        after: u64::MAX,
        wait_ms: u64::MAX,
    });
    assert_request(Operation::Shutdown);
}

#[test]
fn every_nested_request_variant_roundtrips() {
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Inspect,
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Attach {
            spec: ViewSpec {
                size: Size {
                    cols: 17,
                    rows: 17,
                    cell_width: 17,
                    cell_height: 17,
                },
                interactive: true,
                visible: true,
                input: true,
                clipboard: true,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Update {
            view_id: "日本語\0🦀".into(),
            spec: ViewSpec {
                size: Size {
                    cols: 17,
                    rows: 17,
                    cell_width: 17,
                    cell_height: 17,
                },
                interactive: true,
                visible: true,
                input: true,
                clipboard: true,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Detach {
            view_id: "日本語\0🦀".into(),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Poll {
            view_id: "日本語\0🦀".into(),
            after: u64::MAX,
            frame_after: u64::MAX,
            resources: vec![ImageKey {
                id: 17,
                generation: u64::MAX,
            }],
            snapshot_after: u64::MAX,
            wait_ms: u64::MAX,
            row_deltas: true,
            scene_encoding: Some("日本語\0🦀".into()),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Pin {
            view_id: "日本語\0🦀".into(),
            size: Some(Size {
                cols: 17,
                rows: 17,
                cell_width: 17,
                cell_height: 17,
            }),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Input {
            view_id: "日本語\0🦀".into(),
            data: vec![0, 255, 10, 128],
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Paste {
            view_id: "日本語\0🦀".into(),
            data: vec![0, 255, 10, 128],
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::PasteContents {
            view_id: "日本語\0🦀".into(),
            contents: vec![ClipboardContent {
                mime: "日本語\0🦀".into(),
                data: vec![0, 255, 10, 128],
            }],
            location: ClipboardLocation::Primary,
            allow_unsafe: true,
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Key {
            view_id: "日本語\0🦀".into(),
            event: KeyEvent {
                key: "日本語\0🦀".into(),
                modifiers: 17,
                text: "日本語\0🦀".into(),
                action: KeyAction::Repeat,
                unshifted_codepoint: 17,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Mouse {
            view_id: "日本語\0🦀".into(),
            event: MouseEvent {
                x: -1.25,
                y: -1.25,
                modifiers: 17,
                button: MouseButton::Forward,
                action: MouseAction::Motion,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Focus {
            view_id: "日本語\0🦀".into(),
            focused: true,
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Scroll {
            view_id: "日本語\0🦀".into(),
            delta: -9223372036854775807,
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Select {
            view_id: "日本語\0🦀".into(),
            event: SelectionEvent {
                action: SelectionAction::Tick,
                x: -1.25,
                y: -1.25,
                modifiers: 17,
                time_ms: u64::MAX,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::CopySelection {
            view_id: "日本語\0🦀".into(),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Search {
            view_id: "日本語\0🦀".into(),
            action: SearchAction::Query {
                text: "日本語\0🦀".into(),
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Clipboard {
            view_id: "日本語\0🦀".into(),
            request_id: "日本語\0🦀".into(),
            response: ClipboardResponse {
                result: ClipboardResult::IoError,
                contents: vec![ClipboardContent {
                    mime: "日本語\0🦀".into(),
                    data: vec![0, 255, 10, 128],
                }],
                available: vec!["日本語\0🦀".into()],
                remember: true,
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Colors {
            view_id: "日本語\0🦀".into(),
            foreground: [0, 127, 255],
            background: [0, 127, 255],
            cursor: Some([0, 127, 255]),
            palette: Some(vec![[0, 127, 255]]),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Terminate {
            view_id: "日本語\0🦀".into(),
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Shutdown,
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::Whoami,
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::Control {
            request: Box::new(Operation::SessionInfo),
            scope: AgentScope::All,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::SetTitle {
            title: "日本語\0🦀".into(),
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::DiscoverAgents {
            scope: AgentScope::All,
            detail: true,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::SendMessage {
            to: "日本語\0🦀".into(),
            text: "日本語\0🦀".into(),
            idempotency_key: "日本語\0🦀".into(),
            scope: AgentScope::All,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::ReadInbox {
            after: Some("日本語\0🦀".into()),
            limit: Some(usize::MAX),
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::AckMessage {
            ids: vec!["日本語\0🦀".into()],
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::InspectOutbox {
            message_id: Some("日本語\0🦀".into()),
            after: Some("日本語\0🦀".into()),
            limit: Some(usize::MAX),
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::ReceiverRegister {
            session_id: "日本語\0🦀".into(),
            mode: AgentReceiverMode::FutureTurn,
            pid: 17,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::ReceiverSubscribe {
            receiver_id: "日本語\0🦀".into(),
            wait_ms: u64::MAX,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::WakeValidate {
            receiver_id: "日本語\0🦀".into(),
            notification_id: "日本語\0🦀".into(),
            generation: u64::MAX,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "secret".into(),
        request: AgentRequest::WakeOutcome {
            receiver_id: "日本語\0🦀".into(),
            notification_id: "日本語\0🦀".into(),
            generation: u64::MAX,
            outcome: AgentWakeOutcome::Accepted,
        },
    });
    assert_request(Operation::HarnessEvent {
        launch_id: "launch".into(),
        token: "secret".into(),
        event: CaptureEvent::Bind {
            conversation_id: "日本語\0🦀".into(),
            source: ConversationSource::AgyMcp,
        },
    });
    assert_request(Operation::HarnessEvent {
        launch_id: "launch".into(),
        token: "secret".into(),
        event: CaptureEvent::Error {
            message: "日本語\0🦀".into(),
        },
    });
    assert_request(Operation::HarnessEvent {
        launch_id: "launch".into(),
        token: "secret".into(),
        event: CaptureEvent::End,
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Search {
            view_id: "view".into(),
            action: SearchAction::Query {
                text: "日本語\0🦀".into(),
            },
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Search {
            view_id: "view".into(),
            action: SearchAction::Next,
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Search {
            view_id: "view".into(),
            action: SearchAction::Previous,
        },
    });
    assert_request(Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Search {
            view_id: "view".into(),
            action: SearchAction::Clear,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "token".into(),
        request: AgentRequest::WakeOutcome {
            receiver_id: "receiver".into(),
            notification_id: "notification".into(),
            generation: u64::MAX,
            outcome: AgentWakeOutcome::Accepted,
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "token".into(),
        request: AgentRequest::WakeOutcome {
            receiver_id: "receiver".into(),
            notification_id: "notification".into(),
            generation: u64::MAX,
            outcome: AgentWakeOutcome::Failed {
                reason: "日本語\0🦀".into(),
            },
        },
    });
    assert_request(Operation::Agent {
        launch_id: "launch".into(),
        token: "token".into(),
        request: AgentRequest::WakeOutcome {
            receiver_id: "receiver".into(),
            notification_id: "notification".into(),
            generation: u64::MAX,
            outcome: AgentWakeOutcome::Unknown {
                reason: "日本語\0🦀".into(),
            },
        },
    });
    assert_request(Operation::LayoutUpdate {
        workspace_id: "workspace".into(),
        window_id: "window".into(),
        expected_revision: u64::MAX,
        layout: Some(LayoutNode::Split {
            axis: Axis::Vertical,
            ratio: 17,
            first: Box::new(LayoutNode::Leaf {
                pane_id: "日本語\0🦀".into(),
            }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: "日本語\0🦀".into(),
            }),
        }),
    });
}

#[test]
fn frame_admission_rejects_before_reading_payload() {
    let frame = encode_frame(Kind::Request, b"payload").unwrap();
    for (range, replacement) in [
        (0..4, b"NOPE".to_vec()),
        (4..8, (PROTOCOL_VERSION - 1).to_le_bytes().to_vec()),
        (8..9, vec![255]),
        (8..9, vec![Kind::Response as u8]),
        (9..12, vec![0, 1, 0]),
        (12..16, ((MAX_REQUEST + 1) as u32).to_le_bytes().to_vec()),
        (16..24, (1u64 << 63).to_le_bytes().to_vec()),
    ] {
        let mut invalid = frame.clone();
        invalid[range].copy_from_slice(&replacement);
        let mut reader = Cursor::new(invalid);
        assert!(read_frame(&mut reader, Kind::Request, MAX_REQUEST).is_err());
        assert_eq!(reader.position(), HEADER_LEN as u64);
    }
    let mut reader = Cursor::new(frame);
    assert!(read_frame(&mut reader, Kind::Request, 3).is_err());
    assert_eq!(reader.position(), HEADER_LEN as u64);
}

#[test]
fn framed_truncation_is_detected_at_every_boundary_and_coalescing_is_exact() {
    let mut codec = Codec::new();
    let bytes = codec
        .encode_request(&Request::new(Operation::Pane {
            pane_id: "pane".into(),
            request: PaneRequest::Paste {
                view_id: "view".into(),
                data: vec![0, 255, 10, 127],
            },
        }))
        .unwrap();
    let mut frame = encode_frame(Kind::Request, bytes).unwrap();
    for length in 0..frame.len() {
        assert!(
            read_frame(
                &mut Cursor::new(&frame[..length]),
                Kind::Request,
                MAX_REQUEST
            )
            .is_err(),
            "accepted truncation at {length}"
        );
    }
    let first_length = frame.len();
    frame.extend_from_slice(&encode_frame(Kind::ClientEvent, b"next").unwrap());
    let mut reader = Cursor::new(frame);
    let payload = read_frame(&mut reader, Kind::Request, MAX_REQUEST).unwrap();
    decode_request(&payload).unwrap();
    assert_eq!(reader.position(), first_length as u64);
    assert_eq!(
        read_frame(&mut reader, Kind::ClientEvent, MAX_CLIENT_EVENT).unwrap(),
        b"next"
    );
    for length in 0..4 {
        assert!(decode_request(&payload[..length]).is_err());
    }
}

fn root_field(bytes: &[u8], slot: usize) -> (usize, usize) {
    let table = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    table_field(bytes, table, slot)
}

fn table_field(bytes: &[u8], table: usize, slot: usize) -> (usize, usize) {
    let backwards = i32::from_le_bytes(bytes[table..table + 4].try_into().unwrap());
    let vtable = usize::try_from(table as i64 - i64::from(backwards)).unwrap();
    let entry = vtable + 4 + slot * 2;
    let field = table + u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap()) as usize;
    (entry, field)
}

fn indirect(bytes: &[u8], field: usize) -> usize {
    field + u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap()) as usize
}

#[test]
fn required_operations_and_unknown_union_tags_fail_closed() {
    let mut codec = Codec::new();
    let bytes = codec
        .encode_request(&Request::new(Operation::Inspect))
        .unwrap()
        .to_vec();
    let mut absent = bytes.clone();
    let (entry, _) = root_field(&absent, 2);
    absent[entry..entry + 2].fill(0);
    assert!(decode_request(&absent).is_err());
    let mut unknown = bytes;
    let (_, tag) = root_field(&unknown, 1);
    unknown[tag] = 255;
    assert!(decode_request(&unknown).is_err());
}

#[test]
fn malformed_optional_layout_cannot_become_a_layout_clear() {
    let mut codec = Codec::new();
    let request = Request::new(Operation::LayoutUpdate {
        workspace_id: "workspace".into(),
        window_id: "window".into(),
        expected_revision: 7,
        layout: Some(LayoutNode::Leaf {
            pane_id: "pane".into(),
        }),
    });
    let valid = codec.encode_request(&request).unwrap().to_vec();
    let (_, operation) = root_field(&valid, 2);
    let operation = indirect(&valid, operation);
    let (_, wrapper) = table_field(&valid, operation, 3);
    let wrapper = indirect(&valid, wrapper);
    for discriminator in [1, 255] {
        let mut malformed = valid.clone();
        let (_, tag) = table_field(&malformed, wrapper, 0);
        let (entry, _) = table_field(&malformed, wrapper, 1);
        malformed[tag] = discriminator;
        malformed[entry..entry + 2].fill(0);
        let decoded = decode_request(&malformed);
        assert!(
            decoded.is_err(),
            "malformed layout was accepted: {decoded:?}"
        );
    }
}

#[test]
fn malformed_optional_spawn_layout_cannot_become_absent() {
    let mut codec = Codec::new();
    let request = Request::new(Operation::PaneSpawn {
        workspace_id: Some("workspace".into()),
        window_id: Some("window".into()),
        expected_revision: Some(7),
        panes: vec![spawn_entry("new")],
        layout: Some(SpawnLayoutNode::New {
            reference: "new".into(),
        }),
    });
    let valid = codec.encode_request(&request).unwrap().to_vec();
    let (_, operation) = root_field(&valid, 2);
    let operation = indirect(&valid, operation);
    let (_, wrapper) = table_field(&valid, operation, 4);
    let wrapper = indirect(&valid, wrapper);
    for discriminator in [0, 1, 2, 3, 255] {
        for missing_value in [false, true] {
            if !missing_value && matches!(discriminator, 1..=3) {
                continue;
            }
            let mut malformed = valid.clone();
            let (_, tag) = table_field(&malformed, wrapper, 0);
            let (entry, _) = table_field(&malformed, wrapper, 1);
            malformed[tag] = discriminator;
            if missing_value {
                malformed[entry..entry + 2].fill(0);
            }
            assert!(decode_request(&malformed).is_err());
        }
    }
}

#[test]
fn required_spawn_entries_and_recursive_layout_values_fail_closed() {
    let mut codec = Codec::new();
    let request = Request::new(Operation::PaneSpawn {
        workspace_id: Some("workspace".into()),
        window_id: Some("window".into()),
        expected_revision: Some(7),
        panes: vec![spawn_entry("new")],
        layout: Some(SpawnLayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(SpawnLayoutNode::Existing {
                pane_id: "existing".into(),
            }),
            second: Box::new(SpawnLayoutNode::New {
                reference: "new".into(),
            }),
        }),
    });
    let valid = codec.encode_request(&request).unwrap().to_vec();
    let (_, operation) = root_field(&valid, 2);
    let operation = indirect(&valid, operation);
    let (_, entries) = table_field(&valid, operation, 3);
    let entries = indirect(&valid, entries);
    let entry = indirect(&valid, entries + 4);
    let (_, wrapper) = table_field(&valid, operation, 4);
    let wrapper = indirect(&valid, wrapper);
    let (_, layout) = table_field(&valid, wrapper, 1);
    let layout = indirect(&valid, layout);
    let (_, existing) = table_field(&valid, layout, 3);
    let existing = indirect(&valid, existing);
    let (_, new) = table_field(&valid, layout, 5);
    let new = indirect(&valid, new);
    for (table, slot) in [
        (operation, 3), // Required batch vector.
        (entry, 0),     // Required local reference.
        (entry, 1),     // Required spawn specification.
        (layout, 3),    // Required first child value.
        (layout, 5),    // Required second child value.
        (existing, 0),  // Required existing-leaf pane ID.
        (new, 0),       // Required new-leaf reference.
    ] {
        let mut malformed = valid.clone();
        let (entry, _) = table_field(&malformed, table, slot);
        malformed[entry..entry + 2].fill(0);
        assert!(decode_request(&malformed).is_err());
    }
    for slot in [2, 4] {
        let mut malformed = valid.clone();
        let (_, tag) = table_field(&malformed, layout, slot);
        malformed[tag] = 255;
        assert!(decode_request(&malformed).is_err());
    }
}

#[test]
fn spawn_layout_decode_and_encode_obey_aggregate_and_depth_budgets() {
    let mut builder = Builder::new();
    let reference = builder.create_string("x".repeat(64));
    let leaf = g::SpawnLayoutNodeNew::create(&mut builder, reference);
    let mut layout = g::SpawnLayoutNode::create_spawn_layout_node_new(&mut builder, leaf);
    // Repeated offsets are tiny on the wire but must not expand exponentially.
    for _ in 0..10 {
        let split =
            g::SpawnLayoutNodeSplit::create(&mut builder, g::Axis::Horizontal, 500, layout, layout);
        layout = g::SpawnLayoutNode::create_spawn_layout_node_split(&mut builder, split);
    }
    let root = g::OptionalSpawnLayout::create(&mut builder, layout);
    let root = g::OptionalSpawnLayoutRef::read_as_root(builder.finish(root, None)).unwrap();
    assert!(
        operations::read_spawn_layout_node(root.value().unwrap(), &mut Budget::new(1024), 0)
            .is_err()
    );
    let mut layout = SpawnLayoutNode::New {
        reference: "new".into(),
    };
    for _ in 0..130 {
        layout = SpawnLayoutNode::Split {
            axis: Axis::Vertical,
            ratio: 500,
            first: Box::new(SpawnLayoutNode::Existing {
                pane_id: "existing".into(),
            }),
            second: Box::new(layout),
        };
    }
    assert!(
        Codec::new()
            .encode_request(&Request::new(Operation::PaneSpawn {
                workspace_id: Some("workspace".into()),
                window_id: Some("window".into()),
                expected_revision: Some(7),
                panes: vec![spawn_entry("new")],
                layout: Some(layout),
            }))
            .is_err()
    );
}

fn spawn_entry(reference: &str) -> PaneSpawnEntry {
    PaneSpawnEntry {
        reference: reference.into(),
        spec: PaneSpawnSpec {
            argv: vec!["/bin/sh".into()],
            cwd: "/tmp".into(),
            home: "/tmp".into(),
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            title: None,
            isolated_environment: true,
        },
    }
}

#[test]
fn mixed_spawn_layout_and_result_mapping_roundtrip() {
    assert_request(Operation::PaneSpawn {
        workspace_id: Some("workspace".into()),
        window_id: Some("window".into()),
        expected_revision: Some(7),
        panes: vec![spawn_entry("codex"), spawn_entry("claude")],
        layout: Some(SpawnLayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(SpawnLayoutNode::Existing {
                pane_id: "existing".into(),
            }),
            second: Box::new(SpawnLayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 600,
                first: Box::new(SpawnLayoutNode::New {
                    reference: "codex".into(),
                }),
                second: Box::new(SpawnLayoutNode::New {
                    reference: "claude".into(),
                }),
            }),
        }),
    });
    let result = RegistryResult {
        registry: RegistrySnapshot {
            revision: 8,
            workspaces: vec![],
            panes: vec![],
        },
        created_panes: [
            ("codex".into(), "pane-one".into()),
            ("claude".into(), "pane-two".into()),
        ]
        .into(),
        pane_id: None,
        workspace_id: Some("workspace".into()),
        window_id: Some("window".into()),
    };
    let response = Response {
        version: PROTOCOL_VERSION,
        ok: true,
        result: Some(serde_json::to_value(&result).unwrap().into()),
        error: None,
        failure: None,
    };
    let mut codec = Codec::new();
    let decoded = decode_response(codec.encode_response(&response).unwrap()).unwrap();
    let decoded: RegistryResult =
        serde_json::from_value(decoded.result.unwrap().into_json().unwrap()).unwrap();
    assert_eq!(decoded.created_panes, result.created_panes);
    assert!(decoded.pane_id.is_none());
    let mut absent = serde_json::to_value(&result).unwrap();
    absent.as_object_mut().unwrap().remove("created_panes");
    let absent: RegistryResult = serde_json::from_value(absent).unwrap();
    assert!(absent.created_panes.is_empty());
    assert!(
        serde_json::to_value(absent)
            .unwrap()
            .get("created_panes")
            .is_none()
    );
}

#[test]
fn malformed_optional_result_and_failure_cannot_become_absent() {
    let mut codec = Codec::new();
    for (slot, response) in [
        (
            2,
            Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(serde_json::json!({"accepted": true}).into()),
                error: None,
                failure: None,
            },
        ),
        (
            4,
            Response {
                version: PROTOCOL_VERSION,
                ok: false,
                result: None,
                error: Some("closing".into()),
                failure: Some(RequestFailure::SessionClosing {
                    outcome: RequestOutcome::Unknown,
                }),
            },
        ),
    ] {
        let valid = codec.encode_response(&response).unwrap().to_vec();
        let (_, field) = root_field(&valid, slot);
        let wrapper = indirect(&valid, field);
        for discriminator in [1, 255] {
            let mut malformed = valid.clone();
            let (_, tag) = table_field(&malformed, wrapper, 0);
            let (entry, _) = table_field(&malformed, wrapper, 1);
            malformed[tag] = discriminator;
            malformed[entry..entry + 2].fill(0);
            assert!(decode_response(&malformed).is_err());
        }
    }
}

#[test]
fn metadata_numbers_presence_and_typed_failures_are_lossless() {
    let mut metadata = serde_json::Map::new();
    metadata.insert("null".into(), serde_json::Value::Null);
    metadata.insert("signed".into(), i64::MIN.into());
    metadata.insert("unsigned".into(), u64::MAX.into());
    metadata.insert(
        "float".into(),
        serde_json::Number::from_f64(-0.0).unwrap().into(),
    );
    metadata.insert("text".into(), "é\0日本語".into());
    metadata.insert("array".into(), serde_json::json!([false, {}, [], 1.25]));
    let metadata = serde_json::Value::Object(metadata);
    let mut codec = Codec::new();
    for result in [
        None,
        Some(Payload::Control(metadata.clone())),
        Some(Payload::Pane(PanePayload {
            metadata: metadata.clone(),
            scene: None,
        })),
        Some(Payload::Pane(PanePayload {
            metadata: metadata.clone(),
            scene: Some(Vec::new().into()),
        })),
        Some(Payload::Pane(PanePayload {
            metadata: metadata.clone(),
            scene: Some(vec![0, 255, 10].into()),
        })),
    ] {
        for outcome in [
            None,
            Some(RequestOutcome::NotDispatched),
            Some(RequestOutcome::Unknown),
        ] {
            let original = Response {
                version: PROTOCOL_VERSION,
                ok: false,
                result: result.clone(),
                error: Some(String::new()),
                failure: outcome.map(|outcome| RequestFailure::SessionClosing { outcome }),
            };
            let decoded = decode_response(codec.encode_response(&original).unwrap()).unwrap();
            assert_eq!(decoded.ok, original.ok);
            assert_eq!(decoded.error, original.error);
            assert_eq!(decoded.failure, original.failure);
            match (&original.result, &decoded.result) {
                (None, None) => {}
                (Some(Payload::Control(left)), Some(Payload::Control(right))) => {
                    assert_eq!(left, right)
                }
                (Some(Payload::Pane(left)), Some(Payload::Pane(right))) => {
                    assert_eq!(left.metadata, right.metadata);
                    assert_eq!(left.scene, right.scene);
                }
                other => panic!("payload presence/domain changed: {other:?}"),
            }
        }
    }
}

#[test]
fn duplicate_metadata_keys_and_nonfinite_numbers_are_rejected() {
    let mut builder = Builder::new();
    let null = g::NullValue::create(&mut builder);
    let null = g::ValueBody::create_null_value(&mut builder, null);
    let null = g::Value::create(&mut builder, null);
    let key = builder.create_string("duplicate");
    let entry = g::Entry::create(&mut builder, key, null);
    let entries = builder.create_vector([entry, entry]);
    let object = g::ObjectValue::create(&mut builder, entries);
    let object = g::ValueBody::create_object_value(&mut builder, object);
    let root = g::Value::create(&mut builder, object);
    let bytes = builder.finish(root, None);
    let root = g::ValueRef::read_as_root(bytes).unwrap();
    assert!(value::read_value(root, &mut Budget::new(1024), 0).is_err());
    builder.clear();
    let number = g::FloatValue::create(&mut builder, f64::NAN);
    let number = g::ValueBody::create_float_value(&mut builder, number);
    let root = g::Value::create(&mut builder, number);
    let root = g::ValueRef::read_as_root(builder.finish(root, None)).unwrap();
    assert!(value::read_value(root, &mut Budget::new(1024), 0).is_err());
}

#[test]
fn aggregate_work_depth_and_vector_allocation_are_bounded() {
    assert!(Budget::new(1024).vector(usize::MAX, 32).is_err());
    assert!(Budget::new(1024).bytes(1025).is_err());
    assert!(Budget::new(1024).node(129).is_err());
    let mut builder = Builder::new();
    let text = g::StringValue::create(&mut builder, "a".repeat(64));
    let text = g::ValueBody::create_string_value(&mut builder, text);
    let text = g::Value::create(&mut builder, text);
    let values = builder.create_vector([text; 100]);
    let array = g::ArrayValue::create(&mut builder, values);
    let array = g::ValueBody::create_array_value(&mut builder, array);
    let root = g::Value::create(&mut builder, array);
    let root = g::ValueRef::read_as_root(builder.finish(root, None)).unwrap();
    assert!(value::read_value(root, &mut Budget::new(1024), 0).is_err());
    let mut operation = Operation::Inspect;
    for _ in 0..130 {
        operation = Operation::Client {
            authority: ClientAuthority {
                client_id: String::new(),
                token: String::new(),
            },
            request: Box::new(operation),
        };
    }
    assert!(
        Codec::new()
            .encode_request(&Request::new(operation))
            .is_err()
    );
}

#[test]
fn borrowed_client_wrapper_matches_owned_operation_without_payload_clones() {
    let authority = ClientAuthority {
        client_id: "client".into(),
        token: "secret".into(),
    };
    let request = Operation::Pane {
        pane_id: "pane".into(),
        request: PaneRequest::Input {
            view_id: "view".into(),
            data: vec![0, 255, 10],
        },
    };
    let mut codec = Codec::new();
    let decoded =
        decode_request(codec.encode_client_request(&authority, &request).unwrap()).unwrap();
    let expected = Request::new(Operation::Client {
        authority,
        request: Box::new(request),
    });
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn client_events_and_error_absence_roundtrip() {
    let event = ClientEvent {
        event: "detached".into(),
        reason: "会話 ended\0".into(),
    };
    let mut codec = Codec::new();
    assert_eq!(
        decode_client_event(codec.encode_client_event(&event).unwrap()).unwrap(),
        event
    );
    let response = Response {
        version: PROTOCOL_VERSION,
        ok: true,
        result: None,
        error: None,
        failure: None,
    };
    let decoded = decode_response(codec.encode_response(&response).unwrap()).unwrap();
    assert!(
        decoded.ok
            && decoded.result.is_none()
            && decoded.error.is_none()
            && decoded.failure.is_none()
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_spawn_recovery_and_agent_socket_paths_are_raw_and_lossless() {
    use std::os::unix::ffi::OsStringExt;
    let path: std::path::PathBuf =
        std::ffi::OsString::from_vec(b"/tmp/native-\xff\0".to_vec()).into();
    assert_request(Operation::ClientActivate {
        detach_others: false,
        agent_socket: Some(path.clone()),
    });
    assert_request(Operation::PaneResume {
        pane_id: "pane".into(),
        expected: HarnessRecovery {
            kind: HarnessKind::Codex,
            conversation_id: "native-id".into(),
            cwd: path.clone(),
        },
    });
    assert_request(Operation::PaneSpawn {
        workspace_id: None,
        window_id: None,
        expected_revision: None,
        panes: vec![PaneSpawnEntry {
            reference: "raw-path".into(),
            spec: PaneSpawnSpec {
                argv: vec![],
                cwd: path.clone(),
                home: path,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                title: Some(String::new()),
                isolated_environment: false,
            },
        }],
        layout: None,
    });
}

#[test]
fn owned_response_scene_retains_the_receive_allocation_without_copying() {
    let response = Response {
        version: PROTOCOL_VERSION,
        ok: true,
        result: Some(Payload::Pane(PanePayload {
            metadata: serde_json::json!({"revision": u64::MAX}),
            scene: Some(vec![7u8; 1024].into()),
        })),
        error: None,
        failure: None,
    };
    let mut codec = Codec::new();
    let received = codec.encode_response(&response).unwrap().to_vec();
    let start = received.as_ptr() as usize;
    let end = start + received.len();
    let decoded = decode_response_owned(received).unwrap();
    let Some(Payload::Pane(payload)) = decoded.result else {
        panic!("wrong payload domain")
    };
    let scene = payload.scene.unwrap();
    assert_eq!(scene.as_ref(), &[7; 1024]);
    assert!((start..end).contains(&(scene.as_ptr() as usize)));
    assert!(scene.as_ptr() as usize + scene.len() <= end);
}

#[test]
fn invalid_utf8_and_empty_palette_presence_are_not_silently_normalized() {
    let mut codec = Codec::new();
    let mut bytes = codec
        .encode_client_event(&ClientEvent {
            event: "ASCII".into(),
            reason: String::new(),
        })
        .unwrap()
        .to_vec();
    let (_, field) = root_field(&bytes, 0);
    let string = field + u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap()) as usize;
    bytes[string + 4] = 255;
    assert!(decode_client_event(&bytes).is_err());
    for palette in [None, Some(vec![]), Some(vec![[1, 2, 3]])] {
        assert_request(Operation::Pane {
            pane_id: String::new(),
            request: PaneRequest::Colors {
                view_id: String::new(),
                foreground: [0; 3],
                background: [255; 3],
                cursor: None,
                palette,
            },
        });
    }
}
