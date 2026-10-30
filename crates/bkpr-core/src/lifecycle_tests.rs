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
