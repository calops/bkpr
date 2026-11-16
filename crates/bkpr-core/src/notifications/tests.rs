use super::*;
use bkpr_protocol::{
    HarnessKind, Notification, NotificationKind, NotificationScope, ProcessIdentity, SessionInfo,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::atomic::AtomicBool,
};

fn root() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("notifications");
    (directory, root)
}
fn store(root: &Path) -> storage::Store {
    match storage::Store::open(root).unwrap() {
        storage::Opened::Ready(store) => store,
        storage::Opened::Unavailable(_) => panic!("fixture store unexpectedly unavailable"),
    }
}
fn source() -> NotificationSource {
    NotificationSource {
        session: SessionInfo {
            name: "test".into(),
            session_id: "session-a".into(),
            daemon_id: "host-a".into(),
            session_instance_id: "opening-a".into(),
        },
        workspace_id: Some("workspace-a".into()),
        window_id: Some("window-a".into()),
        pane_id: "pane-a".into(),
        harness: HarnessKind::Omp,
        process: ProcessIdentity {
            pid: 42,
            birth: "birth-a".into(),
        },
        launch_id: Some("launch-a".into()),
        binding_generation: Some(1),
    }
}
fn observation(state: Option<&str>) -> NotificationObservation {
    NotificationObservation {
        source: source(),
        state: state.map(str::to_owned),
        kind: (state == Some("blocked")).then(|| "question".into()),
        observed_at_ms: now_ms(),
    }
}
fn health() -> NotificationHealth {
    NotificationHealth {
        healthy: true,
        error: None,
        dropped_observations: 0,
        retained: 0,
    }
}
fn page(store: &mut storage::Store, after: Option<&NotificationCursor>) -> NotificationPage {
    store
        .page(
            &NotificationQuery::default(),
            after,
            false,
            now_ms(),
            health(),
        )
        .unwrap()
}
fn inspect(service: &NotificationService) -> NotificationPage {
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
fn pause(service: &NotificationService) -> Sender<()> {
    let (entered, ready) = bounded(1);
    let (resume, waiting) = bounded(1);
    service
        .handle
        .queue
        .send(Work::Pause(entered, waiting))
        .unwrap();
    ready.recv().unwrap();
    resume
}
fn wait_finished(service: &NotificationService) {
    let shared = &service.handle.shared;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut epoch = shared.epoch.lock().unwrap();
    while !shared.finished.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "fixture worker did not finish");
        epoch = shared
            .changed
            .wait_timeout(epoch, deadline.saturating_duration_since(Instant::now()))
            .unwrap()
            .0;
    }
}

#[test]
fn episodes_deduplicate_semantic_categories_and_require_working_to_rearm() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    for state in [
        "working", "blocked", "blocked", "error", "blocked", "error", "idle", "blocked", "working",
        "blocked",
    ] {
        store.observe(observation(Some(state)), now).unwrap();
    }
    let page = page(&mut store, None);
    assert_eq!(
        page.notifications
            .iter()
            .map(|n| n.kind)
            .collect::<Vec<_>>(),
        [
            NotificationKind::Attention,
            NotificationKind::Error,
            NotificationKind::Attention
        ]
    );
    assert_eq!(
        page.notifications[0].attention_kind.as_deref(),
        Some("question")
    );
    assert_eq!(page.notifications[1].attention_kind, None);
    assert_eq!(page.cursor.sequence, 3);
}

#[test]
fn default_policy_records_attention_idle_and_explicit_done_without_duplicates() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    for state in [
        Some("idle"),
        None,
        Some("blocked"),
        Some("idle"),
        Some("working"),
        Some("idle"),
        Some("idle"),
        Some("done"),
        Some("done"),
        None,
    ] {
        store.observe(observation(state), now).unwrap();
    }
    assert_eq!(
        page(&mut store, None)
            .notifications
            .iter()
            .map(|n| n.kind)
            .collect::<Vec<_>>(),
        [
            NotificationKind::Attention,
            NotificationKind::Idle,
            NotificationKind::Done
        ]
    );
}

#[test]
fn replacement_and_stale_withdrawal_do_not_complete_or_rearm_current_episode() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    store.observe(observation(Some("blocked")), now).unwrap();
    let mut replacement = observation(Some("blocked"));
    replacement.source.process.birth = "birth-b".into();
    replacement.source.binding_generation = Some(2);
    store.observe(replacement.clone(), now).unwrap();
    store.observe(observation(None), now).unwrap();
    store.observe(replacement, now).unwrap();
    let records = page(&mut store, None).notifications;
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|n| n.kind == NotificationKind::Attention)
    );
    assert_eq!(records[1].source.process.birth, "birth-b");
}

#[test]
fn records_sequences_and_policy_survive_restart_but_episode_authority_does_not() {
    let (_directory, root) = root();
    let mut first = store(&root);
    first
        .set_policy(NotificationPolicy {
            scope: NotificationScope {
                session_id: Some("session-a".into()),
                ..Default::default()
            },
            kinds: vec![NotificationKind::Attention, NotificationKind::Done],
        })
        .unwrap();
    first
        .observe(observation(Some("blocked")), now_ms())
        .unwrap();
    let before = page(&mut first, None);
    drop(first);
    let mut reopened = store(&root);
    assert_eq!(reopened.policies().len(), 2);
    reopened
        .observe(observation(Some("blocked")), now_ms())
        .unwrap();
    let restarted = page(&mut reopened, Some(&before.cursor));
    assert_eq!(restarted.notifications.len(), 1);
    assert_eq!(restarted.notifications[0].kind, NotificationKind::Attention);
    reopened
        .observe(observation(Some("working")), now_ms())
        .unwrap();
    reopened
        .observe(observation(Some("done")), now_ms())
        .unwrap();
    let resumed = page(&mut reopened, Some(&restarted.cursor));
    assert_eq!(resumed.cursor.store_id, before.cursor.store_id);
    assert_eq!(
        resumed.notifications[0].sequence,
        restarted.cursor.sequence + 1
    );
    assert_eq!(resumed.notifications[0].kind, NotificationKind::Done);
    assert_eq!(
        resumed.notifications[0].source.session.session_id,
        "session-a"
    );
}

#[test]
fn failed_commit_is_not_a_receipt_and_does_not_consume_episode_or_sequence() {
    let (_directory, root) = root();
    let mut first = store(&root);
    first
        .observe(observation(Some("working")), now_ms())
        .unwrap();
    let cursor = page(&mut first, None).cursor;
    first.fail_writes();
    assert!(
        first
            .observe(observation(Some("blocked")), now_ms())
            .is_err()
    );
    assert!(first.failed());
    first
        .connection_for_test()
        .execute_batch("PRAGMA query_only=OFF")
        .unwrap();
    assert!(
        first
            .observe(observation(Some("blocked")), now_ms())
            .unwrap()
    );
    drop(first);
    let mut reopened = store(&root);
    let resumed = page(&mut reopened, Some(&cursor));
    assert_eq!(resumed.notifications.len(), 1);
    assert_eq!(resumed.notifications[0].sequence, 1);
}

#[test]
fn scoped_policy_overrides_broader_rules_and_rejects_ambiguous_equal_scopes() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let session = NotificationScope {
        session_id: Some("session-a".into()),
        ..Default::default()
    };
    store
        .set_policy(NotificationPolicy {
            scope: session.clone(),
            kinds: vec![],
        })
        .unwrap();
    let workspace = NotificationScope {
        workspace_id: Some("workspace-a".into()),
        ..session.clone()
    };
    store
        .set_policy(NotificationPolicy {
            scope: workspace,
            kinds: vec![NotificationKind::Done],
        })
        .unwrap();
    let ambiguous = NotificationScope {
        pane_id: Some("pane-a".into()),
        ..session.clone()
    };
    assert!(
        store
            .set_policy(NotificationPolicy {
                scope: ambiguous,
                kinds: vec![NotificationKind::Attention]
            })
            .is_err()
    );
    assert!(!store.failed());
    store
        .observe(observation(Some("blocked")), now_ms())
        .unwrap();
    store.observe(observation(Some("done")), now_ms()).unwrap();
    let records = page(&mut store, None).notifications;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, NotificationKind::Done);
    assert!(
        store
            .set_policy(NotificationPolicy {
                scope: NotificationScope {
                    pane_id: Some("unqualified".into()),
                    ..Default::default()
                },
                kinds: vec![]
            })
            .is_err()
    );
}

#[test]
fn policies_have_finite_admission_and_can_replace_at_capacity() {
    let (_directory, root) = root();
    let mut store = store(&root);
    for index in 0..63 {
        store
            .set_policy(NotificationPolicy {
                scope: NotificationScope {
                    session_id: Some(format!("session-{index}")),
                    ..Default::default()
                },
                kinds: vec![],
            })
            .unwrap();
    }
    assert_eq!(store.policies().len(), 64);
    assert!(
        store
            .set_policy(NotificationPolicy {
                scope: NotificationScope {
                    session_id: Some("overflow".into()),
                    ..Default::default()
                },
                kinds: vec![]
            })
            .is_err()
    );
    store.set_policy(NotificationPolicy::default()).unwrap();
    assert_eq!(store.policies().len(), 64);
}

#[test]
fn foreign_and_future_cursors_are_rejected_without_poisoning_storage() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let mut cursor = page(&mut store, None).cursor;
    cursor.sequence += 1;
    assert!(
        store
            .page(
                &NotificationQuery::default(),
                Some(&cursor),
                false,
                now_ms(),
                health()
            )
            .is_err()
    );
    cursor.sequence = 0;
    cursor.store_id = "foreign".into();
    assert!(
        store
            .page(
                &NotificationQuery::default(),
                Some(&cursor),
                false,
                now_ms(),
                health()
            )
            .is_err()
    );
    assert!(!store.failed());
    store
        .observe(observation(Some("blocked")), now_ms())
        .unwrap();
}

#[test]
fn filters_advance_cursors_and_new_inspection_returns_newest_matching_page() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let baseline = page(&mut store, None).cursor;
    for index in 0..4 {
        let mut event = observation(Some("blocked"));
        event.source.pane_id = format!("pane-{index}");
        store.observe(event, now_ms()).unwrap();
    }
    let query = NotificationQuery {
        kinds: vec![NotificationKind::Error],
        ..Default::default()
    };
    let filtered = store
        .page(&query, Some(&baseline), false, now_ms(), health())
        .unwrap();
    assert!(filtered.notifications.is_empty());
    assert_eq!(filtered.cursor.sequence, 4);
    let query = NotificationQuery {
        limit: 2,
        ..Default::default()
    };
    let newest = store.page(&query, None, false, now_ms(), health()).unwrap();
    assert_eq!(
        newest
            .notifications
            .iter()
            .map(|n| n.sequence)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    let first = store
        .page(&query, Some(&baseline), false, now_ms(), health())
        .unwrap();
    assert!(first.has_more);
    assert_eq!(first.cursor.sequence, 2);
    let second = store
        .page(&query, Some(&first.cursor), false, now_ms(), health())
        .unwrap();
    assert_eq!(
        second
            .notifications
            .iter()
            .map(|n| n.sequence)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(!second.has_more);
}

fn seed(store: &mut storage::Store, count: usize, large: bool, now: u64) {
    let mut record = Notification {
        sequence: 1,
        id: "record".into(),
        source: source(),
        kind: NotificationKind::Attention,
        attention_kind: None,
        observed_at_ms: now,
        expires_at_ms: now + storage::MAX_AGE_MS,
    };
    if large {
        record.source.session.name = "n".repeat(1024);
        record.source.session.session_id = "s".repeat(256);
        record.source.session.daemon_id = "d".repeat(256);
        record.source.session.session_instance_id = "i".repeat(256);
        record.source.workspace_id = Some("w".repeat(256));
        record.source.window_id = Some("v".repeat(256));
        record.source.pane_id = "p".repeat(256);
        record.source.process.birth = "b".repeat(256);
        record.source.launch_id = Some("l".repeat(256));
    }
    let transaction = store.connection_for_test().transaction().unwrap();
    for sequence in 1..=count {
        record.sequence = sequence as u64;
        let json = serde_json::to_string(&record).unwrap();
        transaction
            .execute(
                "INSERT INTO notifications VALUES (?1,?2,?3,?4)",
                rusqlite::params![
                    sequence as i64,
                    record.expires_at_ms as i64,
                    json.len() as i64,
                    json
                ],
            )
            .unwrap();
    }
    transaction
        .execute("UPDATE metadata SET sequence=?1", [count as i64])
        .unwrap();
    transaction.commit().unwrap();
}

#[test]
fn count_byte_and_age_retention_preserve_store_head_and_report_gaps() {
    for large in [false, true] {
        let (_directory, root) = root();
        let mut store = store(&root);
        let baseline = page(&mut store, None).cursor;
        let count = if large {
            3000
        } else {
            storage::MAX_RECORDS + 1
        };
        let now = now_ms();
        seed(&mut store, count, large, now);
        let result = store
            .page(
                &NotificationQuery::default(),
                Some(&baseline),
                false,
                now,
                health(),
            )
            .unwrap();
        assert!(result.gap);
        assert!(result.health.retained < count as u64);
        assert!(result.health.retained <= storage::MAX_RECORDS as u64);
        let bytes: usize = store
            .connection_for_test()
            .query_row("SELECT bytes FROM metadata", [], |row| row.get(0))
            .unwrap();
        assert!(bytes <= storage::MAX_BYTES);
        let expired = store
            .page(
                &NotificationQuery::default(),
                Some(&result.cursor),
                false,
                now + storage::MAX_AGE_MS,
                health(),
            )
            .unwrap();
        assert!(expired.gap);
        assert!(expired.notifications.is_empty());
        assert_eq!(expired.cursor.sequence, count as u64);
        assert_eq!(expired.health.retained, 0);
    }
}

#[test]
fn session_requests_clamp_scopes_and_reject_cross_session_policy_writes() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    service.observe(observation(Some("blocked")));
    let mut other = observation(Some("blocked"));
    other.source.session.session_id = "session-b".into();
    service.observe(other);
    let request = NotificationRequest::Inspect {
        query: NotificationQuery::default(),
        after: None,
    };
    let page: NotificationPage = serde_json::from_value(
        service
            .request(request, Some("session-a"), &|| true)
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page.notifications.len(), 1);
    assert_eq!(page.notifications[0].source.session.session_id, "session-a");
    let request = NotificationRequest::Inspect {
        query: NotificationQuery {
            scope: NotificationScope {
                session_id: Some("session-b".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        after: None,
    };
    assert!(
        service
            .request(request, Some("session-a"), &|| true)
            .is_err()
    );
    assert!(
        service
            .request(
                NotificationRequest::PolicySet {
                    policy: NotificationPolicy::default()
                },
                Some("session-a"),
                &|| true
            )
            .is_err()
    );
    service.shutdown().unwrap();
    service.shutdown().unwrap();
}

#[test]
fn new_subscription_establishes_baseline_without_replaying_history() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    service.observe(observation(Some("blocked")));
    let before = inspect(&service);
    let result: NotificationPage = serde_json::from_value(
        service
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: None,
                    wait_ms: 0,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(result.notifications.is_empty());
    assert_eq!(result.cursor, before.cursor);
    service.observe(observation(Some("working")));
    service.observe(observation(Some("error")));
    let resumed: NotificationPage = serde_json::from_value(
        service
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: Some(result.cursor),
                    wait_ms: 0,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resumed.notifications.len(), 1);
    assert_eq!(resumed.notifications[0].kind, NotificationKind::Error);
    service.shutdown().unwrap();
}

#[test]
fn idle_subscriptions_do_not_serialize_queries_and_are_bounded_and_cancellable() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let connected = Arc::new(AtomicBool::new(true));
    let (ready, waiting) = bounded(SUBSCRIPTIONS);
    *service.handle.shared.subscription_waiting.lock().unwrap() = Some(ready);
    let mut workers = Vec::new();
    for _ in 0..SUBSCRIPTIONS {
        let service = service.clone();
        let connected = connected.clone();
        workers.push(thread::spawn(move || {
            service.request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: None,
                    wait_ms: 5000,
                },
                None,
                &|| connected.load(Ordering::Acquire),
            )
        }));
    }
    for _ in 0..SUBSCRIPTIONS {
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert!(
        service
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: None,
                    wait_ms: 0
                },
                None,
                &|| true
            )
            .is_err()
    );
    assert!(
        service
            .request(NotificationRequest::PolicyGet {}, None, &|| true)
            .is_ok()
    );
    connected.store(false, Ordering::Release);
    service.handle.shared.changed.notify_all();
    for worker in workers {
        assert!(worker.join().unwrap().is_err());
    }
    service.shutdown().unwrap();
}

#[test]
fn observation_overflow_reports_loss_and_rejects_saturated_admission() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let resume = pause(&service);
    for _ in 0..QUEUE_CAPACITY + 3 {
        service.observe(observation(Some("working")));
    }
    assert_eq!(service.health().dropped_observations, 3);
    assert!(service.health().healthy);
    assert!(
        service
            .request(NotificationRequest::PolicyGet {}, None, &|| true)
            .is_err()
    );
    resume.send(()).unwrap();
    // A queue barrier, not a sleep, establishes that admitted observations drained.
    let second = pause(&service);
    second.send(()).unwrap();
    service.shutdown().unwrap();
}

#[test]
fn timed_out_shutdown_keeps_writer_authority_and_caches_truthful_outcome() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let resume = pause(&service);
    service.observe(observation(Some("blocked")));
    let first_error = service.shutdown().unwrap_err().to_string();
    assert!(first_error.contains("timed out"));
    assert!(NotificationService::open(&root).is_err());
    resume.send(()).unwrap();
    wait_finished(&service);
    assert_eq!(service.shutdown().unwrap_err().to_string(), first_error);
    let reopened = NotificationService::open(&root).unwrap();
    assert_eq!(inspect(&reopened).notifications.len(), 1);
    reopened.shutdown().unwrap();
}

#[test]
fn corrupt_store_exposes_health_without_fabricating_cursor_or_modifying_bytes() {
    let (_directory, root) = root();
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let db = root.join("notifications.sqlite3");
    fs::write(&db, b"not a SQLite database").unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let service = NotificationService::open(&root).unwrap();
    let health: NotificationHealth = serde_json::from_value(
        service
            .request(NotificationRequest::Health {}, None, &|| true)
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(!health.healthy);
    assert!(
        service
            .request(NotificationRequest::PolicyGet {}, None, &|| true)
            .is_err()
    );
    assert!(
        service
            .request(
                NotificationRequest::Inspect {
                    query: NotificationQuery::default(),
                    after: None
                },
                None,
                &|| true
            )
            .is_err()
    );
    service.observe(observation(Some("blocked")));
    assert_eq!(service.health().dropped_observations, 1);
    assert!(NotificationService::open(&root).is_err());
    assert!(service.shutdown().is_err());
    assert_eq!(fs::read(db).unwrap(), b"not a SQLite database");
}

#[test]
fn storage_rejects_symlinks_hardlinks_and_public_files_without_touching_targets() {
    for mode in ["symlink", "hardlink", "public"] {
        let (directory, root) = root();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let protected = directory.path().join("protected");
        fs::write(&protected, b"keep").unwrap();
        fs::set_permissions(&protected, fs::Permissions::from_mode(0o600)).unwrap();
        let path = root.join("notifications.sqlite3");
        match mode {
            "symlink" => symlink(&protected, &path).unwrap(),
            "hardlink" => fs::hard_link(&protected, &path).unwrap(),
            "public" => {
                fs::write(&path, b"keep").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(NotificationService::open(&root).is_err());
        assert_eq!(fs::read(protected).unwrap(), b"keep");
    }
}

#[test]
fn observation_loss_is_persisted_and_resumed_pages_expose_a_gap() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let baseline = inspect(&service).cursor;
    service.record_dropped_observations(7);
    service.observe(observation(Some("blocked")));
    service.shutdown().unwrap();
    let reopened = NotificationService::open(&root).unwrap();
    assert_eq!(reopened.health().dropped_observations, 7);
    let page: NotificationPage = serde_json::from_value(
        reopened
            .request(
                NotificationRequest::Inspect {
                    query: NotificationQuery::default(),
                    after: Some(baseline),
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(page.gap);
    assert_eq!(page.notifications.len(), 1);
    assert!(page.health.healthy);
    assert_eq!(page.cursor.loss_generation, 7);
    let acknowledged: NotificationPage = serde_json::from_value(
        reopened
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: Some(page.cursor),
                    wait_ms: 0,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(!acknowledged.gap);
    reopened.shutdown().unwrap();
}

#[test]
fn runtime_storage_failure_is_explicit_and_does_not_claim_durable_success() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let connection = rusqlite::Connection::open(root.join("notifications.sqlite3")).unwrap();
    connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
    service.observe(observation(Some("blocked")));
    let resume = pause(&service);
    assert!(!service.health().healthy);
    assert_eq!(service.health().dropped_observations, 1);
    assert!(
        service
            .request(NotificationRequest::PolicyGet {}, None, &|| true)
            .is_err()
    );
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);
    resume.send(()).unwrap();
    assert!(service.shutdown().is_err());
    let reopened = NotificationService::open(&root).unwrap();
    assert!(inspect(&reopened).notifications.is_empty());
    reopened.shutdown().unwrap();
}

#[test]
fn retained_history_expiry_does_not_rearm_a_still_live_attention_episode() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    let old = now - storage::MAX_AGE_MS - 1;
    let mut first = observation(Some("blocked"));
    first.observed_at_ms = old;
    store.observe(first, old).unwrap();
    let mut other = observation(Some("working"));
    other.source.pane_id = "other-pane".into();
    store.observe(other, now).unwrap();
    assert!(!store.observe(observation(Some("blocked")), now).unwrap());
    assert!(page(&mut store, None).notifications.is_empty());
}

#[test]
fn source_capacity_overflow_preserves_existing_episodes_and_does_not_poison_store() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    for index in 0..storage::MAX_EPISODES {
        let mut active = observation(Some("working"));
        active.source.pane_id = format!("pane-{index}");
        store.observe(active, now).unwrap();
    }
    assert!(store.observe(observation(Some("blocked")), now).is_err());
    assert!(!store.failed());
    let mut existing = observation(Some("error"));
    existing.source.pane_id = "pane-0".into();
    store.observe(existing, now).unwrap();
    assert_eq!(
        page(&mut store, None).notifications[0].kind,
        NotificationKind::Error
    );
}

#[test]
fn attention_category_changes_are_distinct_but_repeats_do_not_storm() {
    let (_directory, root) = root();
    let mut store = store(&root);
    for kind in [
        "question",
        "question",
        "permission",
        "auth",
        "permission",
        "auth",
    ] {
        let mut event = observation(Some("blocked"));
        event.kind = Some(kind.into());
        store.observe(event, now_ms()).unwrap();
    }
    let records = page(&mut store, None).notifications;
    assert_eq!(
        records
            .iter()
            .map(|record| record.attention_kind.as_deref())
            .collect::<Vec<_>>(),
        [Some("question"), Some("permission"), Some("auth")]
    );
}

#[test]
fn working_duplicates_and_disabled_notifications_never_require_a_database_write() {
    let (_directory, root) = root();
    let mut store = store(&root);
    store
        .set_policy(NotificationPolicy {
            scope: NotificationScope::default(),
            kinds: vec![],
        })
        .unwrap();
    store.fail_writes();
    for state in ["working", "working", "idle", "done", "working"] {
        assert!(!store.observe(observation(Some(state)), now_ms()).unwrap());
    }
    store.observe(observation(None), now_ms()).unwrap();
    assert!(!store.failed());
}

#[test]
fn expiry_deadline_is_derived_from_retained_data_and_empty_store_has_none() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let now = now_ms();
    assert_eq!(store.next_expiry().unwrap(), None);
    store.observe(observation(Some("blocked")), now).unwrap();
    assert_eq!(
        store.next_expiry().unwrap(),
        Some(now + storage::MAX_AGE_MS)
    );
    assert!(!store.expire(now + storage::MAX_AGE_MS - 1).unwrap());
    assert!(store.expire(now + storage::MAX_AGE_MS).unwrap());
    assert_eq!(store.next_expiry().unwrap(), None);
}

#[test]
fn future_loss_generation_is_rejected_without_poisoning_storage() {
    let (_directory, root) = root();
    let mut store = store(&root);
    let mut cursor = page(&mut store, None).cursor;
    cursor.loss_generation += 1;
    assert!(
        store
            .page(
                &NotificationQuery::default(),
                Some(&cursor),
                false,
                now_ms(),
                health()
            )
            .is_err()
    );
    assert!(!store.failed());
}

#[test]
fn acknowledged_loss_cursor_can_resume_an_idle_subscription_without_repeated_gap() {
    let (_directory, root) = root();
    let service = NotificationService::open(&root).unwrap();
    let baseline = inspect(&service).cursor;
    let resume = pause(&service);
    for _ in 0..QUEUE_CAPACITY + 3 {
        service.observe(observation(Some("working")));
    }
    resume.send(()).unwrap();
    let drained = pause(&service);
    drained.send(()).unwrap();
    let gap: NotificationPage = serde_json::from_value(
        service
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: Some(baseline),
                    wait_ms: 0,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(gap.gap);
    assert_eq!(gap.cursor.loss_generation, 3);
    assert!(gap.health.healthy);
    assert_eq!(gap.health.dropped_observations, 3);
    let baseline: NotificationPage = serde_json::from_value(
        service
            .request(
                NotificationRequest::Subscribe {
                    query: NotificationQuery::default(),
                    after: None,
                    wait_ms: 0,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap(),
    )
    .unwrap();
    assert!(!baseline.gap);
    assert!(baseline.notifications.is_empty());
    assert_eq!(baseline.cursor.loss_generation, 3);
    let (ready, waiting) = bounded(1);
    *service.handle.shared.subscription_waiting.lock().unwrap() = Some(ready);
    let connected = Arc::new(AtomicBool::new(true));
    let connected_worker = connected.clone();
    let subscriber = service.clone();
    let (result, completed) = bounded(1);
    let worker = thread::spawn(move || {
        let reply = subscriber.request(
            NotificationRequest::Subscribe {
                query: NotificationQuery::default(),
                after: Some(gap.cursor),
                wait_ms: 5000,
            },
            None,
            &|| connected_worker.load(Ordering::Acquire),
        );
        result.send(reply).unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        completed.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty)
    ));
    connected.store(false, Ordering::Release);
    service.handle.shared.changed.notify_all();
    assert!(
        completed
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    worker.join().unwrap();
    service.shutdown().unwrap();
}
