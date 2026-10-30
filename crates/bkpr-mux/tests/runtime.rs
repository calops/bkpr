#![cfg(target_os = "linux")]
use bkpr_mux::transport;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    daemon: Child,
}
impl Fixture {
    fn start(script: &str) -> Self {
        let root = std::env::temp_dir().join(format!("pf-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("s");
        let home = root.join("h");
        let daemon = Command::new(env!("CARGO_BIN_EXE_bkpr"))
            .args(["daemon", "--socket"])
            .arg(&socket)
            .arg("--home")
            .arg(&home)
            .args(["--", "/bin/sh", "-c", script])
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            root,
            socket,
            daemon,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.socket.exists() {
            assert!(
                fixture.daemon.try_wait().unwrap().is_none(),
                "owned daemon failed during launch"
            );
            assert!(Instant::now() < deadline, "daemon readiness deadline");
            thread::sleep(Duration::from_millis(5));
        }
        fixture
    }
    fn rpc(&self, request: Value) -> Value {
        let response = transport::request(&self.socket, &request).unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }
    fn attach(&self) -> String {
        self.rpc(json!({"op":"attach","size":{"cols":80,"rows":24,"cell_width":8,"cell_height":16},"interactive":true,"visible":true,"input":true,"clipboard":true}))["view_id"].as_str().unwrap().to_owned()
    }
    fn until(&self, mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "scenario observation deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn keyboard(&self, view: &str, bytes: &[u8]) {
        // One batch, no per-character sleeps: exact decoder -> FIFO RPC sender
        // -> bounded PTY writer path used by the physical TUI.
        let output = Command::new(env!("CARGO_BIN_EXE_bkpr"))
            .args(["keyboard", "--socket"])
            .arg(&self.socket)
            .args(["--view", view])
            .arg(serde_json::to_string(bytes).unwrap())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn home(&self, name: &str) -> PathBuf {
        self.root.join("h").join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = transport::request(&self.socket, &json!({"op":"shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.daemon.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn frame_text(snapshot: &Value) -> String {
    snapshot["frame"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row["cells"].as_array().unwrap())
        .filter_map(|cell| cell["text"].as_str())
        .collect()
}

#[test]
fn stopped_saturated_pty_keeps_program_alive_and_terminates_explicitly() {
    let f =
        Fixture::start("stty -echo -icanon min 1 time 0; echo ready > ready; kill -STOP $$; cat");
    let view = f.attach();
    f.until(|| f.home("ready").exists());
    let pid = f.rpc(json!({"op":"inspect"}))["pid"].as_u64().unwrap() as i32;
    f.until(|| {
        fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with("State:") && line.contains('T'))
    });
    let bytes = vec![b'x'; 1024 * 1024];
    let first = f.rpc(json!({"op":"input","view_id":view,"data":bytes}));
    assert_eq!(first["accepted"], true);
    f.rpc(json!({"op":"input","view_id":view,"data":vec![b'y';1024*1024]}));
    let rejected = transport::request(
        &f.socket,
        &json!({"op":"input","view_id":view,"data":vec![b'z';1024*1024]}),
    )
    .unwrap();
    assert_eq!(rejected["ok"], false);
    assert!(rejected["error"].as_str().unwrap().contains("not accepted"));
    f.until(|| {
        f.rpc(json!({"op":"inspect"}))["writer"]["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["written"].as_u64().unwrap() > 0)
    });
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "normal backpressure must not kill the stopped shell"
    );
    let start = Instant::now();
    f.rpc(json!({"op":"terminate","view_id":view}));
    assert!(start.elapsed() < Duration::from_secs(3));
    let state = f.rpc(json!({"op":"inspect"}));
    assert_eq!(state["lifecycle"], "terminated");
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(
        state["writer"]["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["state"] == "cancelled"
                && r["written"].as_u64().unwrap() > 0
                && r["written"].as_u64().unwrap() < r["total"].as_u64().unwrap())
    );
}
#[test]
fn physical_ascii_csi_u_and_adjacent_editing_reach_real_shell_in_wire_order() {
    let f = Fixture::start("echo ready > ready; exec /bin/sh -i");
    let view = f.attach();
    f.until(|| f.home("ready").exists());
    f.keyboard(&view, b"p\x1b[114;1uintf 'ASCII_FIFO\\n' > ascii-result\r");
    f.until(|| fs::read_to_string(f.home("ascii-result")).ok().as_deref() == Some("ASCII_FIFO\n"));
    f.keyboard(&view, b"printf 'ORDER_OK\\n' > resulX\x1b[D\x1b[3~t\r");
    f.until(|| fs::read_to_string(f.home("result")).ok().as_deref() == Some("ORDER_OK\n"));
    assert!(!f.home("resulX").exists());
}
#[test]
fn isolated_subreaper_reaps_owned_setsid_descendants() {
    let f = Fixture::start(
        "setsid /bin/sh -c 'trap \"\" HUP; echo $$ > escaped.pid; sleep 60 & echo $! > grandchild.pid; wait' & wait",
    );
    let view = f.attach();
    f.until(|| {
        ["escaped.pid", "grandchild.pid"].iter().all(|name| {
            fs::read_to_string(f.home(name))
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .is_some()
        })
    });
    let escaped = fs::read_to_string(f.home("escaped.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let grandchild = fs::read_to_string(f.home("grandchild.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_ne!(
        unsafe { libc::getsid(escaped) },
        f.rpc(json!({"op":"inspect"}))["pid"].as_i64().unwrap() as i32,
        "fixture must actually escape original SID"
    );
    f.rpc(json!({"op":"terminate","view_id":view}));
    assert_eq!(unsafe { libc::kill(escaped, 0) }, -1);
    assert_eq!(unsafe { libc::kill(grandchild, 0) }, -1);
}
#[test]
fn short_lived_output_is_drained_before_exited_is_published() {
    let f = Fixture::start(
        "dd if=/dev/zero bs=4096 count=128 2>/dev/null | tr '\\000' x; printf '\\nFINAL_AFTER_DRAIN\\n'",
    );
    f.until(|| f.rpc(json!({"op":"inspect"}))["lifecycle"] == "exited");
    assert!(frame_text(&f.rpc(json!({"op":"inspect"}))).contains("FINAL_AFTER_DRAIN"));
}
#[test]
fn synchronous_output_timeout_is_scheduled_without_further_output() {
    let f = Fixture::start("printf 'A\\033[?2026hB'; sleep 3");
    let start = Instant::now();
    f.until(|| frame_text(&f.rpc(json!({"op":"inspect"}))).contains("AB"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn isolated_boundary_owns_rapid_reparented_daemon_with_retained_slave_fd() {
    let f = Fixture::start(
        "setsid -f /bin/sh -c 'trap \"\" HUP; sleep 60 & echo $! > orphan.pid'; sleep 60",
    );
    let view = f.attach();
    f.until(|| {
        fs::read_to_string(f.home("orphan.pid"))
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .is_some()
    });
    let pid = fs::read_to_string(f.home("orphan.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    f.until(|| {
        fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| {
                line.strip_prefix("PPid:")
                    .and_then(|s| s.trim().parse::<u32>().ok())
            })
            == Some(f.daemon.id())
    });
    assert_ne!(
        unsafe { libc::getsid(pid) },
        f.rpc(json!({"op":"inspect"}))["pid"].as_i64().unwrap() as i32
    );
    let start = Instant::now();
    f.rpc(json!({"op":"terminate","view_id":view}));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}
