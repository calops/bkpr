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
    let shell_pid = pane.request(Request::Inspect).unwrap()["pid"]
        .as_u64()
        .unwrap() as i32;
    pane.request(Request::Detach { view_id: first }).unwrap();
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
    let second = pane.request(Request::Attach { spec }).unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pane.request(Request::Terminate { view_id: second })
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
        pane.request(Request::Inspect).unwrap()["lifecycle"],
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
        .unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pane.request(Request::Input {
        view_id: view_id.clone(),
        data: b"kept".to_vec(),
    })
    .unwrap();
    while std::fs::read(&input).unwrap() != b"kept" {
        assert!(Instant::now() < deadline, "raw child did not receive input");
        thread::sleep(Duration::from_millis(2));
    }
    pane.request(Request::Terminate { view_id }).unwrap();
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
        let snapshot = pane.request(Request::Inspect).unwrap();
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
