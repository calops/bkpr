use super::*;

#[test]
#[cfg(target_os = "linux")]
fn last_view_detaches_without_killing_and_explicit_termination_reaps_stopped_jobs() {
    let home = std::env::temp_dir().join(format!("bkpr-mux-test-{}", Uuid::new_v4()));
    let size = Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let pane = Pane::spawn(SpawnSpec {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "sleep 60 & echo $! > child.pid; kill -STOP $$; wait".into(),
        ],
        home: home.clone(),
        size,
        isolated_process: false,
        cwd: home.clone(),
        isolated_environment: true,
    })
    .unwrap();
    let spec = ViewSpec {
        size,
        visible: true,
        interactive: true,
        input: true,
        clipboard: false,
    };
    let first = pane
        .request(Request::Attach { spec: spec.clone() })
        .unwrap()
        .into_json()
        .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !home.join("child.pid").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let child_pid = std::fs::read_to_string(home.join("child.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let shell_pid = pane.request(Request::Inspect).unwrap().into_json().unwrap()["pid"]
        .as_u64()
        .unwrap() as i32;
    pane.request(Request::Detach { view_id: first })
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(shell_pid, 0) },
        0,
        "detaching the last projection must preserve the shell"
    );
    assert_eq!(
        unsafe { libc::kill(child_pid, 0) },
        0,
        "detaching must preserve its job"
    );
    let second = pane
        .request(Request::Attach { spec })
        .unwrap()
        .into_json()
        .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pane.request(Request::Terminate { view_id: second })
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(shell_pid, 0) },
        -1,
        "shell must be reaped before terminate returns"
    );
    assert_eq!(
        unsafe { libc::kill(child_pid, 0) },
        -1,
        "stopped-session job must be killed and reaped"
    );
    assert_eq!(
        pane.request(Request::Inspect).unwrap().into_json().unwrap()["lifecycle"],
        "terminated"
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn explicit_termination_does_not_type_eof_into_a_live_raw_child() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let input = home.path().join("input");
    let size = Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let pane = Pane::spawn(SpawnSpec {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "trap '' HUP; stty raw -echo; exec cat > input".into(),
        ],
        home: home.path().to_owned(),
        cwd: home.path().to_owned(),
        size,
        isolated_process: false,
        isolated_environment: true,
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !input.exists() {
        assert!(Instant::now() < deadline, "raw child did not start");
        thread::sleep(Duration::from_millis(2));
    }
    let view_id = pane
        .request(Request::Attach {
            spec: ViewSpec {
                size,
                visible: true,
                interactive: true,
                input: true,
                clipboard: false,
            },
        })
        .unwrap()
        .into_json()
        .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pane.request(Request::Input {
        view_id: view_id.clone(),
        data: b"kept".to_vec(),
    })
    .unwrap()
    .into_json()
    .unwrap();
    while std::fs::read(&input).unwrap() != b"kept" {
        assert!(Instant::now() < deadline, "raw child did not receive input");
        thread::sleep(Duration::from_millis(2));
    }
    pane.request(Request::Terminate { view_id })
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        std::fs::read(input).unwrap(),
        b"kept",
        "termination must signal/reap, not inject a line and EOF"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn final_multibyte_screen_survives_partial_output_batch_at_process_exit() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let expected = "PTY-TAIL-e\u{301}-👨‍👩‍👧‍👦";
    let pane = Pane::spawn(SpawnSpec {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("printf '%65549s' ''; printf '\\033[2J\\033[H%s' '{expected}'"),
        ],
        home: home.path().into(),
        cwd: home.path().into(),
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        isolated_process: false,
        isolated_environment: true,
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut snapshot = loop {
        let snapshot = pane.request(Request::Inspect).unwrap().into_json().unwrap();
        if snapshot["lifecycle"] == "exited" {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "finite output child did not exit"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(snapshot["exit_code"], 0);
    let frame: Frame = serde_json::from_value(snapshot["frame"].take()).unwrap();
    let lines: Vec<String> = frame
        .rows
        .into_iter()
        .map(|row| {
            let mut line: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
            line.truncate(line.trim_end_matches(' ').len());
            line
        })
        .collect();
    let mut expected_lines = vec![String::new(); 24];
    expected_lines[0] = expected.into();
    assert_eq!(lines, expected_lines);
}

#[cfg(target_os = "linux")]
mod live_input {
    use super::*;
    use bkpr_protocol::{KeyAction, PaneSnapshot};

    struct Fixture {
        pane: Pane,
        root: tempfile::TempDir,
        size: Size,
    }

    impl Fixture {
        fn new(modes: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let size = Size {
                cols: 24,
                rows: 4,
                cell_width: 8,
                cell_height: 16,
            };
            let pane = Pane::spawn(SpawnSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!(
                        "stty raw -echo; i=0; while [ \"$i\" -lt 32 ]; do \
                         printf 'history-%02d\\r\\n' \"$i\"; i=$((i+1)); done; \
                         printf '{modes}LIVE'; exec cat > input"
                    ),
                ],
                home: root.path().join("home"),
                cwd: root.path().to_owned(),
                size,
                isolated_process: false,
                isolated_environment: true,
            })
            .unwrap();
            let fixture = Self { pane, root, size };
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let ready = fixture.root.path().join("input").exists()
                    && fixture
                        .pane
                        .shared
                        .published
                        .lock()
                        .unwrap()
                        .frame
                        .as_ref()
                        .is_some_and(|frame| {
                            frame.scrollback_total > 0
                                && frame.rows.iter().any(|row| {
                                    row.cells
                                        .iter()
                                        .map(|cell| cell.text.as_str())
                                        .collect::<String>()
                                        .contains("LIVE")
                                })
                        });
                if ready {
                    break;
                }
                assert!(Instant::now() < deadline, "silent raw pane did not start");
                thread::sleep(Duration::from_millis(2));
            }
            fixture
        }

        fn attach(&self, input: bool, clipboard: bool) -> String {
            self.pane
                .request(Request::Attach {
                    spec: ViewSpec {
                        size: self.size,
                        visible: true,
                        interactive: true,
                        input,
                        clipboard,
                    },
                })
                .unwrap()
                .into_json()
                .unwrap()["view_id"]
                .as_str()
                .unwrap()
                .to_owned()
        }

        fn history(&self, view_id: &str) {
            self.pane
                .request(Request::Scroll {
                    view_id: view_id.into(),
                    delta: i64::MAX,
                })
                .unwrap();
            assert!(self.offset(view_id) > 0);
        }

        fn offset(&self, view_id: &str) -> u64 {
            self.pane.shared.authority.lock().unwrap().views[view_id].scrollback_offset
        }

        fn poll(&self, view_id: &str) -> PaneSnapshot {
            serde_json::from_value(
                self.pane
                    .request(Request::Poll {
                        view_id: view_id.into(),
                        after: 0,
                        frame_after: 0,
                        resources: vec![],
                        snapshot_after: 0,
                        wait_ms: 0,
                        row_deltas: true,
                        scene_encoding: None,
                    })
                    .unwrap()
                    .into_json()
                    .unwrap(),
            )
            .unwrap()
        }

        fn key(view_id: &str, action: KeyAction) -> Request {
            Request::Key {
                view_id: view_id.into(),
                event: KeyEvent {
                    key: "KeyA".into(),
                    modifiers: 0,
                    text: "a".into(),
                    unshifted_codepoint: u32::from('a'),
                    action,
                },
            }
        }

        fn assert_input(&self, expected: &[u8]) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let actual = std::fs::read(self.root.path().join("input")).unwrap();
                if actual == expected {
                    return;
                }
                assert!(
                    actual.len() <= expected.len() && Instant::now() < deadline,
                    "silent child input differs: {actual:?}, expected {expected:?}"
                );
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.pane.request(Request::Shutdown);
        }
    }

    #[test]
    fn accepted_typing_raw_and_paste_return_only_the_target_to_live_without_output() {
        let fixture = Fixture::new("\\033[?2004h");
        let target = fixture.attach(true, true);
        let other = fixture.attach(true, true);
        let live_frame = fixture.poll(&target).frame.unwrap();
        assert_eq!(live_frame.scrollback_offset, 0);
        fixture.history(&other);
        let other_offset = fixture.offset(&other);
        fixture.history(&target);
        let target_offset = fixture.offset(&target);
        let modifier = fixture
            .pane
            .request(Request::Key {
                view_id: target.clone(),
                event: KeyEvent {
                    key: "ShiftLeft".into(),
                    modifiers: 1,
                    text: String::new(),
                    unshifted_codepoint: 0,
                    action: KeyAction::Press,
                },
            })
            .unwrap()
            .into_json()
            .unwrap();
        assert!(modifier["write_id"].is_null());
        assert_eq!(fixture.offset(&target), target_offset);
        let requests = [
            (Fixture::key(&target, KeyAction::Press), b"a".to_vec()),
            (Fixture::key(&target, KeyAction::Repeat), b"a".to_vec()),
            (
                Request::Input {
                    view_id: target.clone(),
                    data: b"\0raw\xff".to_vec(),
                },
                b"\0raw\xff".to_vec(),
            ),
            (
                Request::Paste {
                    view_id: target.clone(),
                    data: b"plain".to_vec(),
                },
                b"\x1b[200~plain\x1b[201~".to_vec(),
            ),
            (
                Request::PasteContents {
                    view_id: target.clone(),
                    contents: vec![ClipboardContent {
                        mime: "text/plain".into(),
                        data: b"enhanced".to_vec(),
                    }],
                    location: ClipboardLocation::Standard,
                    allow_unsafe: false,
                },
                b"\x1b[200~enhanced\x1b[201~".to_vec(),
            ),
        ];
        let mut expected = Vec::new();
        for (request, bytes) in requests {
            fixture.history(&target);
            let before = fixture.poll(&target);
            let other_frame = fixture.poll(&other).frame.unwrap();
            assert!(before.frame.as_ref().unwrap().scrollback_offset > 0);
            assert!(other_frame.rows.iter().any(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("history-00")
            }));
            let accepted = fixture.pane.request(request).unwrap().into_json().unwrap();
            assert_eq!(accepted["accepted"], true);
            assert!(accepted["write_id"].as_u64().is_some());
            assert_eq!(accepted["semantics"], "queued_once_no_replay");
            assert_eq!(fixture.offset(&target), 0);
            assert_eq!(fixture.offset(&other), other_offset);
            let live = fixture.poll(&target);
            let historical = fixture.poll(&other);
            assert!(live.revision > before.revision);
            assert_eq!(live.frame.as_ref().unwrap().scrollback_offset, 0);
            assert_eq!(live.frame.as_ref().unwrap(), &live_frame);
            assert_eq!(
                historical.frame.as_ref().unwrap().scrollback_offset,
                other_offset
            );
            assert_eq!(historical.frame.as_ref().unwrap(), &other_frame);
            expected.extend(bytes);
            fixture.assert_input(&expected);
        }
        let revision = fixture.pane.shared.published.lock().unwrap().revision;
        fixture
            .pane
            .request(Fixture::key(&target, KeyAction::Press))
            .unwrap();
        expected.push(b'a');
        fixture.assert_input(&expected);
        assert_eq!(
            fixture.pane.shared.published.lock().unwrap().revision,
            revision
        );
        fixture
            .pane
            .request(Request::Terminate { view_id: target })
            .unwrap();
        assert_eq!(
            std::fs::read(fixture.root.path().join("input")).unwrap(),
            expected
        );
    }

    #[test]
    fn encoded_release_empty_input_and_denied_input_preserve_history() {
        let fixture = Fixture::new("\\033[>11u");
        let target = fixture.attach(true, false);
        let denied = fixture.attach(false, false);
        fixture.history(&target);
        fixture.history(&denied);
        let target_offset = fixture.offset(&target);
        let denied_offset = fixture.offset(&denied);
        let revision = fixture.pane.shared.published.lock().unwrap().revision;
        let release = fixture
            .pane
            .request(Fixture::key(&target, KeyAction::Release))
            .unwrap()
            .into_json()
            .unwrap();
        assert!(
            release["write_id"].as_u64().is_some(),
            "fixture must encode releases"
        );
        fixture.assert_input(b"\x1b[97;1:3u");
        fixture
            .pane
            .request(Request::Input {
                view_id: target.clone(),
                data: Vec::new(),
            })
            .unwrap();
        assert!(
            fixture
                .pane
                .request(Request::Input {
                    view_id: denied.clone(),
                    data: b"forbidden".to_vec(),
                })
                .is_err()
        );
        assert!(
            fixture
                .pane
                .request(Fixture::key(&denied, KeyAction::Press))
                .is_err()
        );
        assert!(
            fixture
                .pane
                .request(Request::Paste {
                    view_id: denied.clone(),
                    data: b"forbidden".to_vec(),
                })
                .is_err()
        );
        assert!(
            fixture
                .pane
                .request(Request::PasteContents {
                    view_id: target.clone(),
                    contents: vec![ClipboardContent {
                        mime: "text/plain".into(),
                        data: b"no-clipboard-grant".to_vec(),
                    }],
                    location: ClipboardLocation::Standard,
                    allow_unsafe: false,
                })
                .is_err()
        );
        assert_eq!(fixture.offset(&target), target_offset);
        assert_eq!(fixture.offset(&denied), denied_offset);
        assert_eq!(
            fixture.pane.shared.published.lock().unwrap().revision,
            revision
        );
        fixture
            .pane
            .request(Request::Terminate { view_id: target })
            .unwrap();
        assert_eq!(
            std::fs::read(fixture.root.path().join("input")).unwrap(),
            b"\x1b[97;1:3u"
        );
    }

    #[test]
    fn enhanced_paste_event_keeps_its_origin_while_returning_target_to_live() {
        let fixture = Fixture::new("\\033[?5522h");
        let target = fixture.attach(true, true);
        let other = fixture.attach(true, true);
        fixture.history(&target);
        fixture.history(&other);
        let other_offset = fixture.offset(&other);
        let accepted = fixture
            .pane
            .request(Request::PasteContents {
                view_id: target.clone(),
                contents: vec![ClipboardContent {
                    mime: "image/png".into(),
                    data: vec![0, 1, 2, 3],
                }],
                location: ClipboardLocation::Standard,
                allow_unsafe: false,
            })
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(accepted["accepted"], true);
        assert!(accepted["write_id"].as_u64().is_some());
        assert_eq!(fixture.offset(&target), 0);
        assert_eq!(fixture.offset(&other), other_offset);
        let grants = fixture.pane.shared.paste.lock().unwrap();
        assert_eq!(grants.origins.len(), 1);
        assert_eq!(grants.origins.values().next().unwrap().view_id, target);
        drop(grants);
        let deadline = Instant::now() + Duration::from_secs(5);
        let event = loop {
            let bytes = std::fs::read(fixture.root.path().join("input")).unwrap();
            if bytes.ends_with(b"\x1b\\") {
                break bytes;
            }
            assert!(
                Instant::now() < deadline,
                "enhanced paste event did not arrive"
            );
            thread::sleep(Duration::from_millis(2));
        };
        assert!(event.starts_with(b"\x1b]5522;type=read:status=OK"));
        fixture
            .pane
            .request(Request::Terminate { view_id: target })
            .unwrap();
        assert_eq!(
            std::fs::read(fixture.root.path().join("input")).unwrap(),
            event
        );
    }
}
