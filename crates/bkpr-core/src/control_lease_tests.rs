use super::*;
use std::process::{Child, Command as ProcessCommand, Stdio};

struct OwnedSource(Child);
impl OwnedSource {
    fn new() -> Self {
        Self(
            ProcessCommand::new("/bin/sh")
                .args(["-c", "IFS= read -r line"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
    fn process(&self) -> harness_process::Process {
        harness_process::process(self.0.id() as i32)
            .unwrap()
            .unwrap()
    }
    fn exit(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for OwnedSource {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct OwnedTarget(Pane);
impl Drop for OwnedTarget {
    fn drop(&mut self) {
        let _ = self.0.request(Request::Shutdown);
    }
}
fn eventually(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native control fixture did not advance"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn queued_control_input_cannot_outlive_end_retirement_or_either_process_root() {
    use bkpr_protocol::{CaptureEvent, HarnessKind};
    for retirement in ["end", "pane-retirement", "source-exit", "pane-root-exit"] {
        let home = tempfile::tempdir().unwrap();
        let size = Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        // An actual native OSC52 callback blocks this target's terminal worker.
        // The private shell builtin read keeps both source lifetimes stable.
        let mut source = OwnedSource::new();
        let mut root = OwnedSource::new();
        let mut captures = harness::Captures::default();
        let grant = captures
            .insert(
                source.process(),
                root.process(),
                "source-pane".into(),
                HarnessKind::Claude,
            )
            .unwrap();
        let launch = captures.lookup(&grant.launch_id, &grant.token).unwrap();
        let lease = launch.control_lease().unwrap();
        let target = OwnedTarget(
            Pane::spawn(SpawnSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "IFS= read -r start; printf '\\033]52;c;?\\007'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> received; done".into(),
                ],
                home: home.path().into(),
                cwd: home.path().into(),
                size,
                isolated_process: false,
                isolated_environment: true,
            })
            .unwrap(),
        );
        let pane = &target.0;
        let view = pane
            .request(Request::Attach {
                spec: ViewSpec {
                    size,
                    visible: true,
                    interactive: true,
                    input: true,
                    clipboard: true,
                },
            })
            .unwrap()
            .into_json()
            .unwrap()["view_id"]
            .as_str()
            .unwrap()
            .to_owned();
        pane.request(Request::Input {
            view_id: view.clone(),
            data: b"query\n".to_vec(),
        })
        .unwrap();
        eventually(|| pane.shared.clipboard.lock().unwrap().is_some());
        let request = {
            let pane = pane.clone();
            let lease = lease.clone();
            let view_id = view.clone();
            thread::spawn(move || {
                pane.request_control(
                    Request::Input {
                        view_id,
                        data: b"retired-source\n".to_vec(),
                    },
                    &lease,
                )
            })
        };
        // This observes the real public dispatch reaching native command
        // admission, not a mocked command or an eventually-started requester.
        eventually(|| !pane.commands.is_empty());
        assert!(!request.is_finished());
        // A second public request passes initial authorization, then waits on
        // the first request's real serialization mutex. This is the same
        // entered-channel barrier pattern used by registry admission tests.
        let (entered, entered_rx) = mpsc::sync_channel(1);
        *pane.shared.control_request_wait.lock().unwrap() = Some(entered);
        let waiting = {
            let pane = pane.clone();
            let lease = lease.clone();
            let view_id = view.clone();
            thread::spawn(move || {
                pane.request_control(
                    Request::Input {
                        view_id,
                        data: b"waiting-source\n".to_vec(),
                    },
                    &lease,
                )
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!waiting.is_finished());
        match retirement {
            "end" => captures
                .event(&grant.launch_id, &grant.token, CaptureEvent::End)
                .unwrap(),
            "pane-retirement" => captures.retire_pane("source-pane"),
            "source-exit" => source.exit(),
            "pane-root-exit" => root.exit(),
            _ => unreachable!(),
        }
        if matches!(retirement, "end" | "pane-retirement") {
            assert!(!lease.is_valid(), "{retirement} must revoke synchronously");
        }
        // Respond through the real callback bypass while the input request
        // still holds the requests mutex waiting for its native result.
        let pending = pane
            .shared
            .clipboard
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .info
            .clone();
        pane.request(Request::Clipboard {
            view_id: view.clone(),
            request_id: pending.request_id,
            response: ClipboardResponse::denied(),
        })
        .unwrap();
        assert!(request.join().unwrap().is_err(), "{retirement}");
        assert!(waiting.join().unwrap().is_err(), "{retirement}");
        assert!(
            !lease.is_valid(),
            "native execution must observe {retirement}"
        );
        // Keep the destination live and observe real PTY delivery from an
        // independent authority; rejection must not merely terminate it.
        pane.request(Request::Input {
            view_id: view.clone(),
            data: b"surviving-owner\n".to_vec(),
        })
        .unwrap();
        eventually(|| {
            std::fs::read_to_string(home.path().join("received"))
                .is_ok_and(|text| text.contains("surviving-owner"))
        });
        let received = std::fs::read_to_string(home.path().join("received")).unwrap();
        assert!(
            !received.contains("retired-source"),
            "{retirement}: {received:?}"
        );
        assert!(
            !received.contains("waiting-source"),
            "{retirement}: {received:?}"
        );
        assert!(
            pane.request_control(
                Request::Input {
                    view_id: view,
                    data: b"late\n".to_vec(),
                },
                &lease,
            )
            .is_err()
        );
    }
}
