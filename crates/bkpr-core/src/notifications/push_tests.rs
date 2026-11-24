use super::{push_transport::Publisher, *};
use bkpr_protocol::{
    ChannelAttentionKind, HarnessKind, NotificationKind, NotificationScope, ProcessIdentity,
    PushEndpointPolicy, PushRequest, PushRevision, PushSubscription, SessionInfo,
};
use rusqlite::params;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, storage::Store) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let storage::Opened::Ready(store) =
        storage::Store::open(&root.path().canonicalize().unwrap().join("notifications")).unwrap()
    else {
        panic!("private fixture store unavailable")
    };
    (root, store)
}
fn enrollment(id: &str, replaces: Option<PushRevision>) -> PushRequest {
    let publisher = Publisher::new(&Publisher::generate_key().unwrap()).unwrap();
    PushRequest::Enroll {
        request_id: id.into(),
        subscription: PushSubscription {
            endpoint: "https://push.example.test/token-secret".into(),
            p256dh: publisher.public_key(),
            auth: "AAAAAAAAAAAAAAAAAAAAAA".into(),
        },
        policy: PushEndpointPolicy::default(),
        scope: NotificationScope::default(),
        kinds: vec![NotificationKind::Attention],
        replaces,
        device_label: Some("fixture phone".into()),
    }
}
fn nonce(store: &storage::Store, job: &str) -> String {
    let payload: Vec<u8> = store
        .connection
        .query_row("SELECT payload FROM push_jobs WHERE id=?1", [job], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_slice::<Value>(&payload).unwrap()["nonce"]
        .as_str()
        .unwrap()
        .into()
}
fn confirm(store: &mut storage::Store, receipt: &Value, now: u64) {
    let nonce = nonce(store, receipt["job_id"].as_str().unwrap());
    store
        .push_request(
            PushRequest::Confirm {
                destination_id: receipt["destination_id"].as_str().unwrap().into(),
                generation: receipt["generation"].as_u64().unwrap(),
                nonce,
            },
            now,
        )
        .unwrap();
}
fn revision_of(receipt: &Value) -> PushRevision {
    PushRevision {
        destination_id: receipt["destination_id"].as_str().unwrap().into(),
        generation: receipt["generation"].as_u64().unwrap(),
    }
}
fn observation() -> NotificationObservation {
    NotificationObservation {
        source: PaneNotificationSource {
            session: SessionInfo {
                name: "test".into(),
                session_id: "session".into(),
                daemon_id: "host".into(),
                session_instance_id: "opening".into(),
            },
            workspace_id: None,
            window_id: None,
            pane_id: "pane".into(),
            harness: HarnessKind::Omp,
            process: ProcessIdentity {
                pid: 42,
                birth: "birth".into(),
            },
            launch_id: None,
            binding_generation: None,
        },
        state: Some("blocked".into()),
        kind: Some("question".into()),
        observed_at_ms: 1,
    }
}

#[test]
fn challenge_is_not_a_plaintext_receipt_and_enrollment_retry_is_single_flight() {
    let (_root, mut store) = fixture();
    let request = enrollment("enroll", None);
    let receipt = store.push_request(request.clone(), 1).unwrap();
    let proof = nonce(&store, receipt["job_id"].as_str().unwrap());
    assert!(!receipt.to_string().contains(&proof));
    assert!(!format!("{request:?}").contains("token-secret"));
    assert_eq!(receipt, store.push_request(request.clone(), 2).unwrap());
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM push_jobs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        store
            .push_request(
                PushRequest::Confirm {
                    destination_id: receipt["destination_id"].as_str().unwrap().into(),
                    generation: 1,
                    nonce: "wrong".into()
                },
                3
            )
            .is_err()
    );
    let hash: String = store
        .connection
        .query_row("SELECT nonce FROM push_destinations", [], |r| r.get(0))
        .unwrap();
    assert_ne!(hash, proof);
    confirm(&mut store, &receipt, 4);
    let mut changed = request;
    if let PushRequest::Enroll { scope, .. } = &mut changed {
        scope.session_id = Some("different".into());
    }
    assert!(store.push_request(changed, 5).is_err());
}

#[test]
fn rotation_keeps_old_active_until_proof_and_late_expiry_cannot_retire_new_generation() {
    let (_root, mut store) = fixture();
    let first = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &first, 2);
    let old = revision_of(&first);
    let queued = store
        .push_request(
            PushRequest::Test {
                request_id: "old-send".into(),
                destination_id: old.destination_id.clone(),
                generation: old.generation,
            },
            3,
        )
        .unwrap();
    let claimed = store.push_claim(4).unwrap().unwrap();
    assert_eq!(claimed.id, queued["job_id"].as_str().unwrap());
    let next = store
        .push_request(enrollment("rotate", Some(old.clone())), 5)
        .unwrap();
    assert_eq!(first["destination_id"], next["destination_id"]);
    assert!(next["generation"].as_u64().unwrap() > old.generation);
    assert!(
        store
            .push_live(&old.destination_id, old.generation)
            .unwrap()
    );
    confirm(&mut store, &next, 6);
    assert!(
        !store
            .push_live(&old.destination_id, old.generation)
            .unwrap()
    );
    store
        .push_complete(&claimed.id, push_transport::PushSendOutcome::Expired, 7)
        .unwrap();
    let new = revision_of(&next);
    assert!(
        store
            .push_live(&new.destination_id, new.generation)
            .unwrap()
    );
    store
        .push_request(
            PushRequest::Revoke {
                destination_id: old.destination_id,
                generation: old.generation,
            },
            8,
        )
        .unwrap();
    assert!(
        store
            .push_live(&new.destination_id, new.generation)
            .unwrap()
    );
}

#[test]
fn expired_rotation_preserves_old_and_never_reuses_attempt_generation() {
    let (_root, mut store) = fixture();
    let first = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &first, 2);
    let old = revision_of(&first);
    let second = store
        .push_request(enrollment("second", Some(old.clone())), 3)
        .unwrap();
    let proof = nonce(&store, second["job_id"].as_str().unwrap());
    let expires = second["challenge_expires_at_ms"].as_u64().unwrap();
    assert!(
        store
            .push_request(
                PushRequest::Confirm {
                    destination_id: old.destination_id.clone(),
                    generation: second["generation"].as_u64().unwrap(),
                    nonce: proof
                },
                expires
            )
            .is_err()
    );
    assert!(
        store
            .push_live(&old.destination_id, old.generation)
            .unwrap()
    );
    let third = store
        .push_request(enrollment("third", Some(old)), expires + 1)
        .unwrap();
    assert!(third["generation"].as_u64().unwrap() > second["generation"].as_u64().unwrap());
}

#[test]
fn queued_jobs_and_source_record_commit_or_rollback_together() {
    let (_root, mut store) = fixture();
    let destination = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &destination, 2);
    store.connection.execute_batch("CREATE TEMP TRIGGER refuse_push BEFORE INSERT ON push_jobs BEGIN SELECT RAISE(ABORT,'injected durable outbox failure'); END;").unwrap();
    assert!(store.observe(observation(), 3).is_err());
    assert_eq!(store.retained().unwrap(), 0);
    assert_eq!(
        store
            .connection
            .query_row("SELECT sequence FROM metadata", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn per_destination_overflow_is_atomic_visible_loss_not_notification_loss() {
    let (_root, mut store) = fixture();
    let destination = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &destination, 2);
    let rev = revision_of(&destination);
    for index in 0..64 {
        store
            .push_request(
                PushRequest::Test {
                    request_id: format!("test-{index}"),
                    destination_id: rev.destination_id.clone(),
                    generation: rev.generation,
                },
                3,
            )
            .unwrap();
    }
    let request = PushRequest::Test {
        request_id: "overflow".into(),
        destination_id: rev.destination_id.clone(),
        generation: rev.generation,
    };
    assert!(store.push_request(request.clone(), 4).is_err());
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM push_requests WHERE id='overflow'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    assert!(store.observe(observation(), 5).unwrap());
    assert_eq!(store.retained().unwrap(), 1);
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT lost FROM push_destinations WHERE id=?1",
                [&rev.destination_id],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    store.push_expire(400_000).unwrap();
    assert!(store.push_request(request, 400_001).is_ok());
}

#[test]
fn global_overflow_does_not_commit_enrollment_or_failed_idempotency_receipt() {
    let (_root, mut store) = fixture();
    let tx = store.connection.transaction().unwrap();
    for index in 0..512 {
        tx.execute(
            "INSERT INTO push_jobs VALUES(?1,'other',1,'test','dispatching',1,999999,1,x'00',NULL)",
            [format!("job-{index}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let request = enrollment("saturated", None);
    assert!(store.push_request(request.clone(), 2).is_err());
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM push_destinations", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM push_requests", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    store
        .connection
        .execute("DELETE FROM push_jobs", [])
        .unwrap();
    assert!(store.push_request(request, 3).is_ok());
}

#[test]
fn restart_marks_claimed_unknown_and_fairness_leaves_one_send_per_destination() {
    let (root, mut store) = fixture();
    let first = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &first, 2);
    let second = store.push_request(enrollment("second", None), 3).unwrap();
    confirm(&mut store, &second, 4);
    let first = revision_of(&first);
    let second = revision_of(&second);
    for (request_id, rev) in [("one", &first), ("two", &first), ("three", &second)] {
        store
            .push_request(
                PushRequest::Test {
                    request_id: request_id.into(),
                    destination_id: rev.destination_id.clone(),
                    generation: rev.generation,
                },
                5,
            )
            .unwrap();
    }
    let claimed = store.push_claim(6).unwrap().unwrap();
    let other = store.push_claim(6).unwrap().unwrap();
    assert_ne!(claimed.destination_id, other.destination_id);
    assert!(store.push_claim(6).unwrap().is_none());
    drop(store);
    let storage::Opened::Ready(mut reopened) =
        storage::Store::open(&root.path().canonicalize().unwrap().join("notifications")).unwrap()
    else {
        panic!("store restart unavailable")
    };
    let states: Vec<String> = reopened
        .connection
        .prepare("SELECT state FROM push_jobs WHERE id IN (?1,?2)")
        .unwrap()
        .query_map(params![claimed.id, other.id], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(states, vec!["unknown", "unknown"]);
    assert!(reopened.push_claim(7).unwrap().is_some());
}

#[test]
fn worst_case_quoted_name_and_identity_payload_stays_within_transport_bound() {
    let (_root, mut store) = fixture();
    let receipt = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &receipt, 2);
    let mut source = observation();
    source.source.session.name = "\"".repeat(1024);
    source.source.session.session_id = "\\".repeat(256);
    source.source.validate().unwrap();
    assert!(store.observe(source, 3).unwrap());
    let payload: Vec<u8> = store
        .connection
        .query_row(
            "SELECT payload FROM push_jobs WHERE kind='notification'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(payload.len() <= 3000);
    let value: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(value["session_name"], "\"".repeat(1024));
}

#[test]
fn push_management_rejects_session_authority_before_key_or_destination_effects() {
    let (_root, store) = fixture();
    drop(store);
    let service =
        NotificationService::open(&_root.path().canonicalize().unwrap().join("notifications"))
            .unwrap();
    for request in [
        PushRequest::PublicKey {},
        PushRequest::List {},
        PushRequest::Diagnostics {},
        enrollment("denied", None),
    ] {
        assert!(
            service
                .request(
                    NotificationRequest::Push {
                        request: Box::new(request)
                    },
                    Some("session"),
                    &|| true
                )
                .is_err()
        );
    }
    service.shutdown().unwrap();
    drop(service);
    let storage::Opened::Ready(store) =
        storage::Store::open(&_root.path().canonicalize().unwrap().join("notifications")).unwrap()
    else {
        panic!("store unavailable")
    };
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM push_key", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn filter_update_is_idempotent_and_never_replays_history() {
    let (_root, mut store) = fixture();
    assert!(store.observe(observation(), 1).unwrap());
    let receipt = store.push_request(enrollment("enroll", None), 2).unwrap();
    confirm(&mut store, &receipt, 3);
    let old = revision_of(&receipt);
    let queued = store
        .push_request(
            PushRequest::Test {
                request_id: "test".into(),
                destination_id: old.destination_id.clone(),
                generation: old.generation,
            },
            4,
        )
        .unwrap();
    let request = PushRequest::Update {
        request_id: "filters".into(),
        destination_id: old.destination_id.clone(),
        generation: old.generation,
        scope: NotificationScope {
            session_id: Some("another".into()),
            workspace_id: None,
            pane_id: None,
        },
        kinds: vec![NotificationKind::Attention],
    };
    let updated = store.push_request(request.clone(), 5).unwrap();
    assert_eq!(updated["destination_id"], old.destination_id);
    assert!(updated["generation"].as_u64().unwrap() > old.generation);
    assert_eq!(updated, store.push_request(request, 6).unwrap());
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT state FROM push_jobs WHERE id=?1",
                [queued["job_id"].as_str().unwrap()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "revoked"
    );
    assert!(store.push_claim(7).unwrap().is_none());
    assert!(
        !store
            .observe(
                NotificationObservation {
                    state: Some("working".into()),
                    kind: None,
                    ..observation()
                },
                8
            )
            .unwrap()
    );
    assert!(store.observe(observation(), 9).unwrap());
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM push_jobs WHERE kind='notification'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn test_retry_after_restart_is_one_send_and_retry_after_never_fires_early() {
    let (root, mut store) = fixture();
    let receipt = store.push_request(enrollment("enroll", None), 1).unwrap();
    confirm(&mut store, &receipt, 2);
    let rev = revision_of(&receipt);
    let request = PushRequest::Test {
        request_id: "test".into(),
        destination_id: rev.destination_id,
        generation: rev.generation,
    };
    let receipt = store.push_request(request.clone(), 3).unwrap();
    let claimed = store.push_claim(4).unwrap().unwrap();
    store
        .push_complete(
            &claimed.id,
            push_transport::PushSendOutcome::Retry {
                after: Some(Duration::from_secs(120)),
            },
            5,
        )
        .unwrap();
    assert!(store.push_claim(120_004).unwrap().is_none());
    assert!(store.push_claim(120_005).unwrap().is_some());
    drop(store);
    let storage::Opened::Ready(mut store) =
        storage::Store::open(&root.path().canonicalize().unwrap().join("notifications")).unwrap()
    else {
        panic!("store unavailable")
    };
    assert_eq!(receipt, store.push_request(request, 120_006).unwrap());
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM push_jobs WHERE kind='test'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    assert!(store.push_claim(120_007).unwrap().is_none());
}

#[test]
fn full_dispatch_capacity_and_terminal_outcomes_never_create_due_now_timer_spins() {
    let (_root, mut store) = fixture();
    assert_eq!(store.push_deadline(true).unwrap(), None);
    for index in 0..4 {
        let receipt = store
            .push_request(enrollment(&format!("enroll-{index}"), None), 1)
            .unwrap();
        confirm(&mut store, &receipt, 2);
        let rev = revision_of(&receipt);
        for send in 0..2 {
            store
                .push_request(
                    PushRequest::Test {
                        request_id: format!("send-{index}-{send}"),
                        destination_id: rev.destination_id.clone(),
                        generation: rev.generation,
                    },
                    3,
                )
                .unwrap();
        }
    }
    let jobs = (0..4)
        .map(|_| store.push_claim(4).unwrap().unwrap())
        .collect::<Vec<_>>();
    assert!(store.push_claim(4).unwrap().is_none());
    assert!(
        store
            .push_deadline(false)
            .unwrap()
            .is_none_or(|deadline| deadline > 4)
    );
    assert!(
        store
            .push_deadline(true)
            .unwrap()
            .is_none_or(|deadline| deadline > 4)
    );
    for job in jobs {
        store
            .push_complete(
                &job.id,
                push_transport::PushSendOutcome::Unknown {
                    reason: "dispatch_outcome_unknown".into(),
                },
                5,
            )
            .unwrap();
    }
    assert!(
        store
            .push_deadline(false)
            .unwrap()
            .is_none_or(|deadline| deadline > 5)
    );
    assert_eq!(store.push_deadline(true).unwrap(), Some(3));
    for _ in 0..4 {
        let job = store.push_claim(6).unwrap().unwrap();
        store
            .push_complete(
                &job.id,
                push_transport::PushSendOutcome::NotSent {
                    reason: "validation_failed".into(),
                },
                7,
            )
            .unwrap();
    }
    assert!(
        store
            .push_deadline(true)
            .unwrap()
            .is_none_or(|deadline| deadline > 7)
    );
}

#[test]
fn revoke_and_expiry_drop_retired_capabilities_but_keep_the_master_identity() {
    let (_root, mut store) = fixture();
    let public = store.push_request(PushRequest::PublicKey {}, 1).unwrap();
    let receipt = store.push_request(enrollment("enroll", None), 2).unwrap();
    let rev = revision_of(&receipt);
    let listed = store.push_request(PushRequest::List {}, 3).unwrap();
    assert_eq!(listed["destinations"][0]["device_label"], "fixture phone");
    assert_eq!(
        listed["destinations"][0]["scope"],
        serde_json::to_value(NotificationScope::default()).unwrap()
    );
    assert!(!listed.to_string().contains("token-secret"));
    store
        .push_request(
            PushRequest::Revoke {
                destination_id: rev.destination_id.clone(),
                generation: rev.generation,
            },
            4,
        )
        .unwrap();
    let descriptor: String = store
        .connection
        .query_row(
            "SELECT descriptor FROM push_destinations WHERE id=?1",
            [&rev.destination_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&descriptor)
            .unwrap()
            .get("subscription")
            .is_none()
    );
    assert!(!descriptor.contains("token-secret"));
    assert!(descriptor.contains("fixture phone"));
    let fingerprint: String = store
        .connection
        .query_row(
            "SELECT fingerprint FROM push_requests WHERE id='enroll'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fingerprint.len(), 64);
    assert!(!fingerprint.contains("token-secret"));
    let payload: Vec<u8> = store
        .connection
        .query_row(
            "SELECT payload FROM push_jobs WHERE id=?1",
            [receipt["job_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(payload.is_empty());
    assert_eq!(
        public,
        store.push_request(PushRequest::PublicKey {}, 5).unwrap()
    );
    let pending = store.push_request(enrollment("expires", None), 6).unwrap();
    store
        .push_expire(pending["challenge_expires_at_ms"].as_u64().unwrap())
        .unwrap();
    let (descriptor, nonce): (String, Option<String>) = store
        .connection
        .query_row(
            "SELECT descriptor,nonce FROM push_destinations WHERE id=?1",
            [pending["destination_id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&descriptor)
            .unwrap()
            .get("subscription")
            .is_none()
    );
    assert!(!descriptor.contains("token-secret"));
    assert!(nonce.is_none());
}

#[test]
fn shutdown_after_claim_is_unknown_and_never_automatically_replayed() {
    let (_root, mut store) = fixture();
    let receipt = store.push_request(enrollment("enroll", None), 1).unwrap();
    let job = store.push_claim(2).unwrap().unwrap();
    store.push_interrupt().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT state FROM push_jobs WHERE id=?1", [&job.id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "unknown"
    );
    assert!(store.push_claim(3).unwrap().is_none());
    let proof: Option<String> = store
        .connection
        .query_row(
            "SELECT nonce FROM push_destinations WHERE id=?1",
            [receipt["destination_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(proof.is_some());
}

#[test]
fn receipt_retention_releases_capacity_at_the_explicit_horizon() {
    let (_root, mut store) = fixture();
    let receipt = store.push_request(enrollment("enroll", None), 1).unwrap();
    confirm(&mut store, &receipt, 2);
    let rev = revision_of(&receipt);
    let request = PushRequest::Test {
        request_id: "reusable-after-horizon".into(),
        destination_id: rev.destination_id,
        generation: rev.generation,
    };
    let original = store.push_request(request.clone(), 3).unwrap();
    let horizon = original["idempotency_expires_at_ms"].as_u64().unwrap();
    assert_eq!(
        original,
        store.push_request(request.clone(), horizon - 1).unwrap()
    );
    let next = store.push_request(request, horizon).unwrap();
    assert_ne!(original["job_id"], next["job_id"]);
    assert!(next["idempotency_expires_at_ms"].as_u64().unwrap() > horizon);
}

#[test]
fn generation_highwater_survives_retired_row_compaction() {
    let (_root, mut store) = fixture();
    let receipt = store.push_request(enrollment("first", None), 1).unwrap();
    confirm(&mut store, &receipt, 2);
    let rev = revision_of(&receipt);
    let expired = store
        .push_request(enrollment("expired", Some(rev.clone())), 3)
        .unwrap();
    let deadline = expired["challenge_expires_at_ms"].as_u64().unwrap();
    store.push_expire(deadline).unwrap();
    for index in 0..129 {
        store
            .connection
            .execute(
                "INSERT INTO push_destinations VALUES(?1,1,'expired',0,NULL,'',0)",
                [format!("retired-{index}")],
            )
            .unwrap();
    }
    store.push_expire(deadline + 1).unwrap();
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM push_destinations WHERE id=?1 AND generation=?2",
                params![rev.destination_id, expired["generation"].as_u64().unwrap()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    let next = store
        .push_request(enrollment("next", Some(rev)), deadline + 2)
        .unwrap();
    assert!(next["generation"].as_u64().unwrap() > expired["generation"].as_u64().unwrap());
}

#[test]
fn expired_endpoint_renews_the_same_device_with_retained_nonsecret_settings() {
    let (_root, mut store) = fixture();
    let original = store.push_request(enrollment("original", None), 1).unwrap();
    confirm(&mut store, &original, 2);
    let old = revision_of(&original);
    store
        .push_request(
            PushRequest::Test {
                request_id: "expires".into(),
                destination_id: old.destination_id.clone(),
                generation: old.generation,
            },
            3,
        )
        .unwrap();
    let job = store.push_claim(4).unwrap().unwrap();
    store
        .push_complete(&job.id, push_transport::PushSendOutcome::Expired, 5)
        .unwrap();
    let listed = store.push_request(PushRequest::List {}, 6).unwrap();
    assert_eq!(listed["destinations"][0]["state"], "expired");
    assert_eq!(listed["destinations"][0]["device_label"], "fixture phone");
    assert_eq!(
        listed["destinations"][0]["kinds"],
        serde_json::json!(["attention"])
    );
    assert_eq!(
        listed["destinations"][0]["scope"],
        serde_json::to_value(NotificationScope::default()).unwrap()
    );
    assert!(!listed.to_string().contains("token-secret"));
    assert!(store.observe(observation(), 7).unwrap());
    let mut request = enrollment("renew", Some(old));
    if let PushRequest::Enroll { device_label, .. } = &mut request {
        *device_label = None;
    }
    let renewed = store.push_request(request, 8).unwrap();
    assert_eq!(renewed["destination_id"], original["destination_id"]);
    assert!(renewed["generation"].as_u64().unwrap() > original["generation"].as_u64().unwrap());
    confirm(&mut store, &renewed, 9);
    assert!(store.push_claim(10).unwrap().is_none());
    let listed = store.push_request(PushRequest::List {}, 11).unwrap();
    assert_eq!(listed["destinations"][0]["device_label"], "fixture phone");
    let current = revision_of(&renewed);
    store
        .push_request(
            PushRequest::Revoke {
                destination_id: current.destination_id.clone(),
                generation: current.generation,
            },
            12,
        )
        .unwrap();
    assert!(
        store
            .push_request(enrollment("revoked-renewal", Some(current)), 13)
            .is_err()
    );
}

#[test]
fn empty_filters_mute_automatic_events_without_losing_the_verified_test_route() {
    let (_root, mut store) = fixture();
    let original = store.push_request(enrollment("original", None), 1).unwrap();
    confirm(&mut store, &original, 2);
    let old = revision_of(&original);
    let muted = store
        .push_request(
            PushRequest::Update {
                request_id: "mute".into(),
                destination_id: old.destination_id,
                generation: old.generation,
                scope: NotificationScope::default(),
                kinds: vec![],
            },
            3,
        )
        .unwrap();
    assert!(store.observe(observation(), 4).unwrap());
    assert!(store.push_claim(5).unwrap().is_none());
    let listed = store.push_request(PushRequest::List {}, 6).unwrap();
    assert_eq!(listed["destinations"][0]["enabled"], false);
    let muted = revision_of(&muted);
    store
        .push_request(
            PushRequest::Test {
                request_id: "muted-test".into(),
                destination_id: muted.destination_id.clone(),
                generation: muted.generation,
            },
            7,
        )
        .unwrap();
    let test = store.push_claim(8).unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&test.payload).unwrap()["type"],
        "test"
    );
    store
        .push_complete(&test.id, push_transport::PushSendOutcome::Accepted, 9)
        .unwrap();
    let active = store
        .push_request(
            PushRequest::Update {
                request_id: "enable".into(),
                destination_id: muted.destination_id,
                generation: muted.generation,
                scope: NotificationScope::default(),
                kinds: vec![NotificationKind::Attention],
            },
            10,
        )
        .unwrap();
    assert!(store.push_claim(11).unwrap().is_none());
    assert!(
        !store
            .observe(
                NotificationObservation {
                    state: Some("working".into()),
                    kind: None,
                    ..observation()
                },
                12
            )
            .unwrap()
    );
    assert!(store.observe(observation(), 13).unwrap());
    let event = store.push_claim(14).unwrap().unwrap();
    assert_eq!(event.generation, active["generation"].as_u64().unwrap());
    assert_eq!(
        serde_json::from_slice::<Value>(&event.payload).unwrap()["type"],
        "notification"
    );
}

#[test]
fn channel_attention_receipt_record_and_scoped_push_commit_together() {
    let (_root, mut store) = fixture();
    let event = tests::channel_source("push-message", ChannelAttentionKind::Report);
    for (request_id, pane_id) in [("workspace-phone", None), ("pane-phone", Some("pane-a"))] {
        let mut request = enrollment(request_id, None);
        let PushRequest::Enroll { scope, .. } = &mut request else {
            unreachable!()
        };
        *scope = NotificationScope {
            session_id: Some(event.session.session_id.clone()),
            workspace_id: Some(event.workspace_id.clone()),
            pane_id: pane_id.map(str::to_owned),
        };
        let receipt = store.push_request(request, 1).unwrap();
        confirm(&mut store, &receipt, 2);
    }
    store
        .connection
        .execute_batch(
            "CREATE TEMP TRIGGER fail_channel_job BEFORE INSERT ON push_jobs
             WHEN NEW.kind='notification' BEGIN SELECT RAISE(ABORT, 'fixture'); END;",
        )
        .unwrap();
    assert!(store.record_channel_attention(event.clone(), 3, 3).is_err());
    assert_eq!(store.retained().unwrap(), 0);
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM channel_attention_receipts", [], |r| {
                r.get::<_, u64>(0)
            })
            .unwrap(),
        0
    );
    store
        .connection
        .execute_batch("DROP TRIGGER fail_channel_job")
        .unwrap();
    assert!(store.record_channel_attention(event.clone(), 3, 4).unwrap());
    assert!(!store.record_channel_attention(event, 3, 5).unwrap());
    assert_eq!(store.retained().unwrap(), 1);
    let mut statement = store
        .connection
        .prepare("SELECT payload FROM push_jobs WHERE kind='notification'")
        .unwrap();
    let payloads: Vec<Vec<u8>> = statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(payloads.len(), 1);
    let payload: Value = serde_json::from_slice(&payloads[0]).unwrap();
    assert_eq!(payload["session_id"], "session-a");
    assert_eq!(payload["kind"], "attention");
    assert_eq!(payload["attention_kind"], Value::Null);
    assert!(!payload.to_string().contains("push-message"));
}
