use super::*;

fn birth(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}
struct StopPane(Pane);
impl Drop for StopPane {
    fn drop(&mut self) {
        let _ = self.0.request(Request::Shutdown);
    }
}
struct OwnedOrphan {
    pid: i32,
    birth: u64,
}
impl Drop for OwnedOrphan {
    fn drop(&mut self) {
        // Failure cleanup is exact-birth scoped; never kill an unrelated reused PID.
        if birth(self.pid) == Some(self.birth) {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }
}
fn spawn(home: PathBuf, argv: Vec<String>) -> StopPane {
    StopPane(
        Pane::spawn(SpawnSpec {
            cwd: home.clone(),
            home,
            argv,
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            isolated_process: false,
            isolated_environment: true,
        })
        .unwrap(),
    )
}

// This subprocess fixture deliberately exits without returning to the Rust harness.
// The parent regression invokes it explicitly; the default test pass does not fork here.
#[test]
#[ignore = "subprocess fixture exercised by quick_root_exit_reaps_stopped_other_pgrp_without_touching_parallel_pane"]
fn quick_exit_fork_fixture() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join("fork-now").exists() {
        assert!(
            Instant::now() < deadline,
            "parent never released fork fixture"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let mut pipe = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        // Only async-signal-safe operations between fork and permanent stop/exit.
        unsafe {
            libc::close(pipe[0]);
            if libc::setpgid(0, 0) != 0
                || libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR
            {
                libc::_exit(91);
            }
            if libc::write(pipe[1], b"r".as_ptr().cast(), 1) != 1 {
                libc::_exit(92);
            }
            libc::close(pipe[1]);
            libc::raise(libc::SIGSTOP);
            loop {
                libc::pause();
            }
        }
    }
    unsafe {
        libc::close(pipe[1]);
    }
    let mut ready = 0u8;
    assert_eq!(
        unsafe { libc::read(pipe[0], (&mut ready as *mut u8).cast(), 1) },
        1
    );
    unsafe {
        libc::close(pipe[0]);
    }
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child, &mut status, libc::WUNTRACED) },
        child
    );
    assert!(libc::WIFSTOPPED(status));
    let child_birth = birth(child).unwrap();
    let pgrp = unsafe { libc::getpgid(child) };
    let session = unsafe { libc::getsid(child) };
    let root = std::process::id();
    std::fs::write(
        home.join("owned-child.tmp"),
        format!("{child} {child_birth} {pgrp} {session} {root}\n"),
    )
    .unwrap();
    std::fs::rename(home.join("owned-child.tmp"), home.join("owned-child")).unwrap();
    // No sleep or periodic-observation window: leave the stopped job behind immediately.
    unsafe {
        libc::_exit(0);
    }
}

#[test]
fn quick_root_exit_reaps_stopped_other_pgrp_without_touching_parallel_pane() {
    let root = tempfile::tempdir().unwrap();
    let parallel = spawn(
        root.path().join("parallel"),
        vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
    );
    let parallel_pid = parallel.0.request(Request::Inspect).unwrap()["pid"]
        .as_i64()
        .unwrap() as i32;
    let parallel_birth = birth(parallel_pid).unwrap();
    let home = root.path().join("quick-exit");
    prepare_home(&home).unwrap();
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "process_exit_tests::quick_exit_fork_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let raw = command.spawn().unwrap();
    let root_pid = raw.id();
    let mut exiting = OwnedChild {
        inner: Box::new(raw),
        pid: Some(root_pid),
        cleaned: false,
        last_observed: Instant::now(),
        tree: Some(process_tree::Tree::new(root_pid, false).unwrap()),
        exit_observed: false,
    };
    // Deterministically create the job after Tree::new; deliberately perform no timer
    // observation before polling/reaping the root, reproducing the natural-exit gap.
    std::fs::write(home.join("fork-now"), b"fork").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        if let Ok(record) = std::fs::read_to_string(home.join("owned-child")) {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "fork fixture did not establish its stopped job"
        );
        thread::sleep(Duration::from_millis(5));
    };
    let fields = record
        .split_whitespace()
        .map(|part| part.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let child = OwnedOrphan {
        pid: fields[0] as i32,
        birth: fields[1],
    };
    assert_eq!(
        fields[0], fields[2],
        "job must own a different process group"
    );
    assert_eq!(
        fields[3], fields[4],
        "job must remain in its pane leader's session"
    );
    assert_ne!(fields[0], fields[4]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = exiting.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "root did not exit");
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(status.exit_code(), 0);
    exiting.terminate().unwrap();
    assert_ne!(
        birth(child.pid),
        Some(child.birth),
        "quick-root-exit leaked the stopped same-session job"
    );
    assert_eq!(
        birth(parallel_pid),
        Some(parallel_birth),
        "cleanup touched another pane's execution"
    );
    assert_eq!(
        parallel.0.request(Request::Inspect).unwrap()["lifecycle"],
        "running"
    );
    for _ in 0..3 {
        assert_eq!(
            exiting.try_wait().unwrap().unwrap().exit_code(),
            0,
            "draining must use cached status without probing a reaped/reusable PID"
        );
    }
}
