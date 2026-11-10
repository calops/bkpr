use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const TARGET: &str = "22222222-2222-4222-8222-222222222222";

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn event(time_ms: u64) -> Event {
    Event {
        time_ms,
        operation: "pane.terminate",
        target: OpaqueId::new(TARGET),
        outcome: "success",
    }
}
fn lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn local_sink_writes_body_free_events_and_counts_rejected_data() {
    let root = private_root();
    let (recorder, guard) = start(root.path(), SESSION, FILE_BYTES).unwrap();
    recorder.record("workspace.rename", Some(TARGET), "success");
    recorder.record("agent.wake", Some(TARGET), "unknown");
    recorder.record("contains a prompt body", None, "success");
    recorder.record(
        "agent.send",
        Some("a-capability-or-private-message"),
        "success",
    );
    // Local fixture initialization must not install the process-wide daemon sink.
    guard.signal_stop();
    guard.done.recv().unwrap();
    let report = guard.recorder.counts.report();
    assert!(report.stopped);
    assert_eq!(report.written, 2);
    assert_eq!(report.dropped, 2);
    assert_eq!(report.io_failures, 0);
    assert_eq!(report.pending, 0);
    let path = root.path().join("diagnostics/events.jsonl");
    let records = lines(&path);
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["operation"], "workspace.rename");
    assert_eq!(records[0]["session_id"], SESSION);
    assert_eq!(records[0]["target_id"], TARGET);
    assert_eq!(records[1]["outcome"], "unknown");
    assert_eq!(records[2]["dropped_total"], 2);
    for record in &records {
        let object = record.as_object().unwrap();
        assert!(object.keys().all(|key| matches!(
            key.as_str(),
            "time_ms"
                | "session_id"
                | "operation"
                | "target_id"
                | "outcome"
                | "dropped_total"
                | "io_failures_total"
        )));
    }
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.path().join("diagnostics"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn rotation_keeps_complete_recent_records_with_fixed_byte_and_file_bounds() {
    let root = private_root();
    let mut writer = PrivateFiles::open(root.path(), 512).unwrap();
    let session = OpaqueId::new(SESSION).unwrap();
    let counts = Counts::default();
    for i in 0..100 {
        writer.event(&session, event(i), &counts).unwrap();
    }
    let directory = root.path().join("diagnostics");
    let entries: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect();
    assert_eq!(entries.len(), 4);
    let mut total = 0;
    for entry in entries {
        let metadata = entry.metadata().unwrap();
        assert!(metadata.len() <= 512);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        total += metadata.len();
        assert!(!lines(&entry.path()).is_empty());
    }
    assert!(total <= 4 * 512);
    assert_eq!(
        lines(&directory.join("events.jsonl")).last().unwrap()["time_ms"],
        99
    );
}

#[test]
fn queue_pressure_is_nonblocking_and_loss_is_measurable() {
    let (queue, receiver) = bounded(QUEUE_EVENTS);
    let counts = Arc::new(Counts {
        accepting: AtomicBool::new(true),
        ..Counts::default()
    });
    let recorder = Recorder {
        queue,
        counts: counts.clone(),
    };
    for _ in 0..QUEUE_EVENTS + 7 {
        recorder.record("pane.create", None, "success");
    }
    let report = counts.report();
    assert_eq!(receiver.len(), QUEUE_EVENTS);
    assert_eq!(report.pending, QUEUE_EVENTS as u64);
    assert_eq!(report.dropped, 7);
    // Fixed-size queue entries, not just a count limit over arbitrary byte strings.
    assert!(std::mem::size_of::<Event>() * receiver.len() <= 64 * 1024);
    drop(receiver);
    recorder.record("pane.create", None, "success");
    assert_eq!(counts.report().dropped, 8);
}

#[test]
fn unsafe_root_directory_and_file_links_never_touch_their_targets() {
    let fixture = private_root();
    let external = fixture.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(external.join("config"), b"keep user configuration").unwrap();
    fs::set_permissions(external.join("config"), fs::Permissions::from_mode(0o600)).unwrap();
    let root_link = fixture.path().join("root-link");
    symlink(&external, &root_link).unwrap();
    assert!(PrivateFiles::open(&root_link, 512).is_err());
    assert!(!external.join("diagnostics").exists());

    for name in ["diagnostics", "events.jsonl", "events.jsonl.2"] {
        let root = private_root();
        if name == "diagnostics" {
            symlink(&external, root.path().join(name)).unwrap();
        } else {
            let directory = root.path().join("diagnostics");
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            symlink(external.join("config"), directory.join(name)).unwrap();
        }
        assert!(
            PrivateFiles::open(root.path(), 512).is_err(),
            "accepted unsafe {name}"
        );
        assert_eq!(
            fs::read(external.join("config")).unwrap(),
            b"keep user configuration"
        );
    }
}

#[test]
fn unsafe_permissions_and_hard_links_disable_logging_without_modifying_files() {
    let root = private_root();
    let mut writer = PrivateFiles::open(root.path(), 512).unwrap();
    writer
        .event(
            &OpaqueId::new(SESSION).unwrap(),
            event(1),
            &Counts::default(),
        )
        .unwrap();
    drop(writer);
    let path = root.path().join("diagnostics/events.jsonl");
    let content = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(PrivateFiles::open(root.path(), 512).is_err());
    assert_eq!(fs::read(&path).unwrap(), content);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&path, root.path().join("unrelated")).unwrap();
    assert!(PrivateFiles::open(root.path(), 512).is_err());
    assert_eq!(fs::read(&path).unwrap(), content);
}

#[test]
fn startup_removes_only_oversized_owned_log_slots_after_all_checks_pass() {
    let root = private_root();
    drop(PrivateFiles::open(root.path(), 512).unwrap());
    let directory = root.path().join("diagnostics");
    fs::write(directory.join("events.jsonl"), vec![b'x'; 4096]).unwrap();
    let unrelated = directory.join("unrelated");
    fs::write(&unrelated, b"not a diagnostic slot").unwrap();
    drop(PrivateFiles::open(root.path(), 512).unwrap());
    assert_eq!(
        fs::metadata(directory.join("events.jsonl")).unwrap().len(),
        0
    );
    assert_eq!(fs::read(&unrelated).unwrap(), b"not a diagnostic slot");

    fs::write(directory.join("events.jsonl"), vec![b'x'; 4096]).unwrap();
    symlink(&unrelated, directory.join("events.jsonl.3")).unwrap();
    assert!(PrivateFiles::open(root.path(), 512).is_err());
    assert_eq!(
        fs::metadata(directory.join("events.jsonl")).unwrap().len(),
        4096
    );
    assert_eq!(fs::read(&unrelated).unwrap(), b"not a diagnostic slot");
}

#[test]
fn rotation_failure_drops_diagnostics_not_the_control_operation() {
    let root = private_root();
    let writer = PrivateFiles::open(root.path(), 512).unwrap();
    let protected = root.path().join("protected");
    fs::write(&protected, b"leave untouched").unwrap();
    symlink(&protected, root.path().join("diagnostics/events.jsonl.1")).unwrap();
    let (queue, receiver) = bounded(QUEUE_EVENTS);
    let (stop, stopping) = bounded(1);
    let counts = Arc::new(Counts {
        accepting: AtomicBool::new(true),
        ..Counts::default()
    });
    let recorder = Recorder {
        queue,
        counts: counts.clone(),
    };
    for _ in 0..10 {
        recorder.record("pane.terminate", Some(TARGET), "success");
    }
    stop.try_send(()).unwrap();
    run(
        writer,
        OpaqueId::new(SESSION).unwrap(),
        receiver,
        stopping,
        &counts,
    );
    let report = counts.report();
    assert!(report.io_failures > 0);
    assert_eq!(report.written + report.dropped, 10);
    assert_eq!(report.pending, 0);
    recorder.record("workspace.close", None, "success");
    assert_eq!(counts.report().dropped, report.dropped + 1);
    assert_eq!(fs::read(protected).unwrap(), b"leave untouched");
}

#[test]
fn checked_directory_descriptor_survives_path_replacement_without_following_links() {
    let root = private_root();
    let mut writer = PrivateFiles::open(root.path(), 512).unwrap();
    let held = root.path().join("held-diagnostics");
    fs::rename(root.path().join("diagnostics"), &held).unwrap();
    let external = private_root();
    symlink(external.path(), root.path().join("diagnostics")).unwrap();
    for i in 0..20 {
        writer
            .event(
                &OpaqueId::new(SESSION).unwrap(),
                event(i),
                &Counts::default(),
            )
            .unwrap();
    }
    assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);
    assert_eq!(
        lines(&held.join("events.jsonl")).last().unwrap()["time_ms"],
        19
    );
}
