use super::*;
use crate::context::test_support::{Guard, Session};
use bkpr::context_contract::{ContextPane, ContextWorkspace};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

struct ContextFixture {
    session: Session,
    sequence: Cell<u64>,
    panes: RefCell<BTreeMap<String, ContextPane>>,
}

impl ContextFixture {
    fn publish(&self) {
        let panes: Vec<_> = self.panes.borrow().values().cloned().collect();
        let workspaces: BTreeMap<_, _> = panes
            .iter()
            .map(|pane| {
                (
                    pane.workspace_id.clone(),
                    ContextWorkspace {
                        id: pane.workspace_id.clone(),
                        name: pane.workspace_id.clone(),
                    },
                )
            })
            .collect();
        self.publish_as(workspaces.into_values().collect(), panes);
    }

    fn publish_as(&self, workspaces: Vec<ContextWorkspace>, panes: Vec<ContextPane>) {
        let sequence = self.sequence.get() + 1;
        self.session.publish(sequence, workspaces, panes).unwrap();
        self.sequence.set(sequence);
    }
}

thread_local! {
    static CONTEXT_FIXTURES: RefCell<BTreeMap<PathBuf, Weak<ContextFixture>>> = const { RefCell::new(BTreeMap::new()) };
}

struct FixtureDirectory {
    // Drop the lookup override before removing the underlying session files.
    _guard: Guard,
    context: Rc<ContextFixture>,
    directory: tempfile::TempDir,
}

impl std::ops::Deref for FixtureDirectory {
    type Target = tempfile::TempDir;
    fn deref(&self) -> &Self::Target {
        &self.directory
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        CONTEXT_FIXTURES.with(|fixtures| {
            fixtures.borrow_mut().remove(self.directory.path());
        });
    }
}

fn fixture(broker: &Broker) -> Rc<ContextFixture> {
    CONTEXT_FIXTURES.with(|fixtures| {
        fixtures
            .borrow()
            .get(&broker.directory)
            .and_then(Weak::upgrade)
            .unwrap()
    })
}

fn broker() -> (FixtureDirectory, Broker) {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let context = Rc::new(ContextFixture {
        session: Session::new("relay test").unwrap(),
        sequence: Cell::new(0),
        panes: RefCell::new(BTreeMap::new()),
    });
    let guard = context.session.guard();
    CONTEXT_FIXTURES.with(|fixtures| {
        fixtures
            .borrow_mut()
            .insert(directory.path().to_owned(), Rc::downgrade(&context));
    });
    let broker = Broker::new(Store::default(), directory.path().to_owned());
    (
        FixtureDirectory {
            _guard: guard,
            context,
            directory,
        },
        broker,
    )
}

fn register(broker: &mut Broker, workspace: &str) -> Request {
    let workspace = broker.directory.join(workspace);
    fs::create_dir_all(&workspace).unwrap();
    let request = Request {
        protocol: PROTOCOL_VERSION,
        agent_id: random_hex(16).unwrap(),
        token: random_hex(32).unwrap(),
        method: "register".into(),
        params: json!({"cwd":workspace,"repository":workspace,"harness":"omp","pid":std::process::id()}),
    };
    broker.perform(&request).unwrap();
    let context = fixture(broker);
    let pane_id = format!("terminal_{}", context.panes.borrow().len() + 1);
    context.panes.borrow_mut().insert(
        request.agent_id.clone(),
        ContextPane {
            pane_id,
            workspace_id: workspace.to_string_lossy().into_owned(),
            pane_name: format!("{} pane", request.agent_id),
            agent_id: Some(request.agent_id.clone()),
        },
    );
    context.publish();
    request
}

fn as_request(agent: &Request, method: &str, params: Value) -> Request {
    Request {
        protocol: PROTOCOL_VERSION,
        agent_id: agent.agent_id.clone(),
        token: agent.token.clone(),
        method: method.into(),
        params,
    }
}

fn send(
    broker: &mut Broker,
    sender: &Request,
    recipient: &Request,
    key: &str,
) -> (Value, Vec<WakeJob>) {
    broker
        .perform(&as_request(
            sender,
            "send",
            json!({"to":recipient.agent_id,"text":key,"idempotency_key":key}),
        ))
        .unwrap()
}

fn connect_receiver(broker: &mut Broker, recipient: &Request) -> mpsc::Receiver<Value> {
    let (sender, receiver) = mpsc::channel();
    broker
        .subscribers
        .entry(recipient.agent_id.clone())
        .or_default()
        .push(Subscriber {
            id: "test".to_owned(),
            sender,
        });
    receiver
}

fn ready_receiver(broker: &mut Broker, recipient: &Request) -> mpsc::Receiver<Value> {
    let receiver = connect_receiver(broker, recipient);
    broker
        .perform(&as_request(
            recipient,
            "receiver",
            json!({"kind":"omp_mcp","pid":std::process::id(),"activity":"unknown"}),
        ))
        .unwrap();
    receiver
}

#[test]
fn credentials_scope_and_actor_boundaries_survive_retained_receipts() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "two");
    let wrong = Request {
        token: "0".repeat(64),
        ..as_request(&alice, "inbox", json!({}))
    };
    assert!(broker.perform(&wrong).is_err());
    let local = broker
        .perform(&as_request(&alice, "discover", json!({})))
        .unwrap()
        .0;
    assert!(local["agents"].as_array().unwrap().is_empty());
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"hello","idempotency_key":"hello"})
            ))
            .is_err()
    );
    let (response, _) = broker.perform(&as_request(&alice, "send",
        json!({"to":bob.agent_id,"text":"hello","scope":"all","idempotency_key":"hello","from":"spoof"}))).unwrap();
    let id = response["message_id"].as_str().unwrap();
    assert_eq!(broker.store.messages[id].from, alice.agent_id);
    assert!(
        broker
            .perform(&as_request(&alice, "ack", json!({"ids":[id]})))
            .is_err()
    );
    assert!(
        broker
            .perform(&as_request(&bob, "outbox", json!({"message_id":id})))
            .is_err()
    );
    let (inbox, _) = broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    assert_eq!(inbox["messages"][0]["text"], "hello");
    let (outbox, _) = broker
        .perform(&as_request(&alice, "outbox", json!({"message_id":id})))
        .unwrap();
    assert_eq!(outbox["message"]["state"], "read");
    assert!(outbox["message"]["acknowledged_at"].is_null());
    assert!(outbox["message"].get("text").is_none());
    broker
        .perform(&as_request(&bob, "ack", json!({"ids":[id]})))
        .unwrap();
    assert!(broker.store.messages[id].text.is_none());
    assert!(
        broker
            .perform(&as_request(&alice, "ack", json!({"ids":[id]})))
            .is_err()
    );
    assert_eq!(
        broker
            .perform(&as_request(&bob, "ack", json!({"ids":[id]})))
            .unwrap()
            .0["acknowledged"],
        0
    );
    assert_eq!(
        broker
            .perform(&as_request(&alice, "outbox", json!({"message_id":id})))
            .unwrap()
            .0["message"]["state"],
        "processed"
    );
}

#[test]
fn idempotency_is_sender_scoped_payload_exact_and_survives_ack_and_reload() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let carol = register(&mut broker, "one");
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"missing key"})
            ))
            .is_err()
    );
    let (first, _) = send(&mut broker, &alice, &bob, "stable");
    let id = first["message_id"].as_str().unwrap();
    broker
        .perform(&as_request(&bob, "ack", json!({"ids":[id]})))
        .unwrap();
    broker.store = load_store(directory.path()).unwrap();
    let (duplicate, jobs) = send(&mut broker, &alice, &bob, "stable");
    assert_eq!(duplicate["message_id"], first["message_id"]);
    assert_eq!(duplicate["duplicate"], true);
    assert!(jobs.is_empty());
    for changed in [
        json!({"to":bob.agent_id,"text":"different","idempotency_key":"stable"}),
        json!({"to":carol.agent_id,"text":"stable","idempotency_key":"stable"}),
        json!({"to":bob.agent_id,"text":"stable","idempotency_key":"stable","scope":"all"}),
        json!({"to":bob.agent_id,"text":"stable","idempotency_key":"stable","scope":"session"}),
        json!({"to":bob.agent_id,"text":"stable","idempotency_key":"stable","scope":"repository"}),
    ] {
        assert!(
            broker
                .perform(&as_request(&alice, "send", changed))
                .is_err()
        );
    }
    let (other_sender, _) = send(&mut broker, &carol, &bob, "stable");
    assert_ne!(other_sender["message_id"], first["message_id"]);
    assert_eq!(broker.store.messages.len(), 2);
}

#[test]
fn concurrent_retries_commit_only_one_message() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let state = Arc::new(Mutex::new(state));
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let state = Arc::clone(&state);
            let request = as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"once","idempotency_key":"once","scope":"repository"}),
            );
            std::thread::spawn(move || {
                state.lock().perform(&request).unwrap().0["message_id"].clone()
            })
        })
        .collect();
    let ids: Vec<_> = requests
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(state.lock().store.messages.len(), 1);
}

#[test]
fn exit_retention_starts_on_observation_not_creation_and_keeps_receipts() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let (response, _) = send(&mut broker, &alice, &bob, "retained body");
    let id = response["message_id"].as_str().unwrap();
    broker.store.messages.get_mut(id).unwrap().created_at = 0;
    let observed = now();
    broker.maintain(observed).unwrap();
    assert!(broker.store.messages[id].text.is_some());
    broker.store.agents.get_mut(&bob.agent_id).unwrap().pid = 0;
    broker.maintain(observed).unwrap();
    assert_eq!(broker.store.agents[&bob.agent_id].exited_at, Some(observed));
    broker.maintain(observed + RETENTION_MS - 1).unwrap();
    assert!(broker.store.messages[id].text.is_some());
    assert_eq!(broker.next_expiry(), Some(observed + RETENTION_MS));
    broker.maintain(observed + RETENTION_MS).unwrap();
    assert!(broker.store.messages[id].text.is_none());
    assert_eq!(
        broker.store.messages[id].expired_at,
        Some(observed + RETENTION_MS)
    );
    assert!(broker.next_expiry().is_none());
    assert!(broker.store.agents.contains_key(&bob.agent_id));
    assert!(
        broker
            .perform(&as_request(&bob, "inbox", json!({})))
            .is_err()
    );
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"new","idempotency_key":"new"})
            ))
            .is_err()
    );
    let (retry, _) = send(&mut broker, &alice, &bob, "retained body");
    assert_eq!(retry["message_id"], response["message_id"]);
    assert_eq!(retry["status"]["state"], "expired");
    let saved = load_store(directory.path()).unwrap();
    assert!(saved.messages[id].text.is_none());
    assert!(!saved.messages[id].payload_digest.is_empty());
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("retained body")
    );
    assert!(
        broker
            .perform(&as_request(&bob, "register", bob.params.clone()))
            .is_err()
    );
}

#[test]
fn legacy_json_migrates_without_dropping_text_or_backdating_exit() {
    let (directory, mut broker) = broker();
    let cwd = directory.path();
    let legacy = json!({"agents":{"old":{
        "id":"old","token":"old-token","harness":"codex","workspace":cwd,"cwd":cwd,
        "title":"old launch","pid":0,"created_at":0,"session_id":null,
        "queue_endpoint":null,"codex_program":null,"accepting":true}},
        "messages":{"message":{"id":"message","from":"sender","to":"old","text":"unread legacy","created_at":0}}});
    broker.store = serde_json::from_value(legacy).unwrap();
    let observed = now();
    broker.maintain(observed).unwrap();
    assert_eq!(broker.store.agents["old"].exited_at, Some(observed));
    assert_eq!(
        broker.store.messages["message"].text.as_deref(),
        Some("unread legacy")
    );
    let saved = load_store(directory.path()).unwrap();
    assert_eq!(
        saved.messages["message"].text.as_deref(),
        Some("unread legacy")
    );
    assert!(!saved.messages["message"].payload_digest.is_empty());
    broker.maintain(observed + RETENTION_MS).unwrap();
    assert!(broker.store.messages["message"].text.is_none());
    assert_eq!(broker.store.messages["message"].delivery.state, "expired");
}

#[test]
fn wake_coalescing_and_old_callbacks_are_generation_guarded() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    let (first, jobs) = send(&mut broker, &alice, &bob, "first");
    let old = &jobs[0];
    broker.outcome(old, WakeOutcome::Accepted).unwrap();
    let (second, jobs) = send(&mut broker, &alice, &bob, "second");
    assert!(jobs.is_empty());
    assert_eq!(second["status"]["delivery"]["state"], "coalesced");
    let (read, jobs) = broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    assert_eq!(read["messages"].as_array().unwrap().len(), 2);
    assert!(jobs.is_empty());
    assert!(
        broker.store.messages[first["message_id"].as_str().unwrap()]
            .acknowledged_at
            .is_none()
    );
    let (third, jobs) = send(&mut broker, &alice, &bob, "third");
    let new = &jobs[0];
    assert!(new.generation > old.generation);
    broker
        .outcome(old, WakeOutcome::Unknown("late old callback".to_owned()))
        .unwrap();
    assert_eq!(
        broker.store.agents[&bob.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .generation,
        new.generation
    );
    assert_eq!(
        broker.store.agents[&bob.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .state,
        "pending"
    );
    broker
        .perform(&as_request(
            &bob,
            "ack",
            json!({"ids":[first["message_id"],second["message_id"]]}),
        ))
        .unwrap();
    assert_eq!(
        broker.store.agents[&bob.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .id,
        third["message_id"]
    );
    broker
        .outcome(
            new,
            WakeOutcome::Unknown("dispatch outcome unavailable".to_owned()),
        )
        .unwrap();
    let (_, jobs) = send(&mut broker, &alice, &bob, "fourth");
    assert!(jobs.is_empty());
    assert_eq!(
        broker.store.agents[&bob.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .state,
        "unknown"
    );
}

#[test]
fn failed_wake_is_durable_and_new_messages_do_not_blindly_retry() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    let (first, jobs) = send(&mut broker, &alice, &bob, "first");
    broker
        .outcome(
            &jobs[0],
            WakeOutcome::Failed("executable unavailable".to_owned()),
        )
        .unwrap();
    let (_, jobs) = send(&mut broker, &alice, &bob, "second");
    assert!(jobs.is_empty());
    let saved = load_store(directory.path()).unwrap();
    let message = &saved.messages[first["message_id"].as_str().unwrap()];
    assert_eq!(message.delivery.state, "failed");
    assert_eq!(
        message.delivery.reason.as_deref(),
        Some("executable unavailable")
    );
    assert!(message.text.is_some());
    let observed = now();
    broker.store.agents.get_mut(&bob.agent_id).unwrap().pid = 0;
    broker.maintain(observed).unwrap();
    broker.maintain(observed + RETENTION_MS).unwrap();
    let saved = load_store(directory.path()).unwrap();
    let message = &saved.messages[first["message_id"].as_str().unwrap()];
    assert!(message.text.is_none());
    assert_eq!(
        message.wake_failure.as_ref().unwrap().reason,
        "executable unavailable"
    );
}

#[test]
fn pagination_notifies_unread_pages_and_accepts_acknowledged_cursor() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    for index in 0..11 {
        let (response, _) = broker.perform(&as_request(&alice, "send", json!({
            "to":bob.agent_id,"text":String::from_utf8(vec![0;MAX_TEXT]).unwrap(),"idempotency_key":index.to_string()
        }))).unwrap();
        broker
            .store
            .messages
            .get_mut(response["message_id"].as_str().unwrap())
            .unwrap()
            .created_at = index;
    }
    let (first, jobs) = broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    assert_eq!(first["messages"].as_array().unwrap().len(), 10);
    assert_eq!(first["has_more"], true);
    assert_eq!(jobs.len(), 1);
    assert!(serde_json::to_vec(&first).unwrap().len() < MAX_FRAME);
    let ids: Vec<_> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    broker
        .perform(&as_request(&bob, "ack", json!({"ids":ids})))
        .unwrap();
    let (_, arrivals) = send(&mut broker, &alice, &bob, "arrived during processing");
    assert!(arrivals.is_empty());
    let (last, jobs) = broker
        .perform(&as_request(
            &bob,
            "inbox",
            json!({"after":first["next_cursor"]}),
        ))
        .unwrap();
    assert_eq!(last["messages"].as_array().unwrap().len(), 2);
    assert_eq!(last["has_more"], false);
    assert!(jobs.is_empty());
    assert!(broker.store.agents[&bob.agent_id].notification.is_none());
}

#[test]
fn read_before_dispatch_cancels_obsolete_notification_without_stranding_arrival() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let receiver = ready_receiver(&mut state, &bob);
    let (_, obsolete) = send(&mut state, &alice, &bob, "first");
    state
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    let (_, current) = send(&mut state, &alice, &bob, "new arrival");
    let state = Arc::new(Mutex::new(state));
    dispatch_jobs(&state, obsolete).unwrap();
    assert!(receiver.try_recv().is_err());
    dispatch_jobs(&state, current).unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap()["event"],
        "message"
    );
}

#[test]
fn unix_subscription_pushes_one_opaque_pointer_until_receiver_reads() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let (first, jobs) = send(&mut state, &alice, &bob, "private body");
    assert!(jobs.is_empty());
    assert_eq!(first["status"]["delivery"]["state"], "blocked");
    state
        .perform(&as_request(
            &bob,
            "receiver",
            json!({"kind":"omp_mcp","pid":std::process::id(),"activity":"unknown"}),
        ))
        .unwrap();
    let state = Arc::new(Mutex::new(state));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let server_state = Arc::clone(&state);
    let server = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(&mut client, &json!({"protocol":PROTOCOL_VERSION,"agent_id":bob.agent_id,"token":bob.token,"method":"subscribe"})).unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap()["event"], "ready");
    let event = read_frame(&mut reader).unwrap().unwrap();
    assert_eq!(event["message"]["id"], first["message_id"]);
    assert!(event["message"].get("text").is_none());
    assert!(event["message"].get("token").is_none());
    let (_, jobs) = send(&mut state.lock(), &alice, &bob, "coalesced body");
    assert!(jobs.is_empty());
    assert_eq!(state.lock().store.messages.len(), 2);
    drop(reader);
    server.join().unwrap();
    assert!(state.lock().subscribers[&bob.agent_id].is_empty());
}

#[test]
fn extension_readiness_is_separate_and_stale_clear_is_ignored() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let (who, _) = broker
        .perform(&as_request(&alice, "whoami", json!({})))
        .unwrap();
    assert_eq!(who["readiness"]["registered"], true);
    assert_eq!(who["readiness"]["native_bound"], false);
    assert_eq!(who["readiness"]["receiver_alive"], false);
    let _receiver = connect_receiver(&mut broker, &alice);
    let (who, _) = broker
        .perform(&as_request(&alice, "whoami", json!({})))
        .unwrap();
    assert_eq!(who["readiness"]["notification_connection"], true);
    assert_eq!(who["readiness"]["receiver_alive"], false);
    assert_eq!(who["readiness"]["can_notify"], false);
    broker
        .perform(&as_request(
            &alice,
            "receiver",
            json!({"kind":"omp_mcp","pid":std::process::id(),"activity":"busy"}),
        ))
        .unwrap();
    let (who, _) = broker
        .perform(&as_request(&alice, "whoami", json!({})))
        .unwrap();
    assert_eq!(who["readiness"]["receiver_alive"], true);
    assert_eq!(who["readiness"]["receiver"]["activity"], "busy");
    assert_eq!(who["readiness"]["active_turn_interruption"], false);
    assert_eq!(who["readiness"]["can_notify"], true);
    broker
        .perform(&as_request(
            &alice,
            "receiver",
            json!({"kind":"omp_mcp","pid":0,"enabled":false}),
        ))
        .unwrap();
    assert!(receiver_alive(
        &broker.store.agents[&alice.agent_id].receiver
    ));
    broker
        .perform(&as_request(
            &alice,
            "receiver",
            json!({"kind":"omp_mcp","pid":std::process::id(),"enabled":false}),
        ))
        .unwrap();
    assert!(!receiver_alive(
        &broker.store.agents[&alice.agent_id].receiver
    ));
}

#[test]
fn snapshots_refuse_symlinks_public_modes_and_nonregular_files() {
    let (directory, mut broker) = broker();
    register(&mut broker, "one");
    let state = directory.path().join("state.json");
    assert!(load_store(directory.path()).is_ok());
    fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_store(directory.path()).is_err());
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let actual = directory.path().join("actual.json");
    fs::rename(&state, &actual).unwrap();
    std::os::unix::fs::symlink(&actual, &state).unwrap();
    assert!(load_store(directory.path()).is_err());
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    assert!(load_store(directory.path()).is_err());
}

#[test]
fn diagnostics_expose_no_credentials_or_bodies_and_hide_expired_snapshot_body() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let (response, _) = send(&mut broker, &alice, &bob, "secret body");
    let value = snapshot_status(&broker.store);
    let encoded = serde_json::to_string(&value).unwrap();
    assert!(!encoded.contains(&alice.token));
    assert!(!encoded.contains("secret body"));
    let live = serde_json::to_string(&broker.diagnostics()).unwrap();
    assert!(!live.contains(&alice.token));
    assert!(!live.contains("secret body"));
    assert!(
        value["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a.get("token").is_none())
    );
    let id = response["message_id"].as_str().unwrap();
    broker
        .store
        .agents
        .get_mut(&bob.agent_id)
        .unwrap()
        .exited_at = Some(now() - RETENTION_MS);
    let summary = snapshot_summary(&broker.store, &broker.store.messages[id]);
    assert_eq!(summary["state"], "expired");
    assert_eq!(summary["body_available"], false);
}

#[test]
fn failed_commit_does_not_retain_uncommitted_message() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    broker.directory = directory.path().join("missing");
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"not committed","idempotency_key":"not committed"})
            ))
            .is_err()
    );
    assert!(broker.store.messages.is_empty());
}

#[test]
fn incompatible_clients_get_protocol_errors_without_mutation() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let old = Request {
        protocol: 2,
        ..as_request(&alice, "lease", json!({"pid":std::process::id()}))
    };
    assert!(
        broker
            .perform(&old)
            .err()
            .unwrap()
            .to_string()
            .contains("incompatible relay protocol")
    );
    assert_eq!(broker.store.agents.len(), 1);
    assert!(broker.store.messages.is_empty());
}

#[test]
fn connection_without_extension_readiness_stays_blocked_until_handshake() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let receiver = connect_receiver(&mut state, &bob);
    let (first, jobs) = send(&mut state, &alice, &bob, "first");
    assert!(jobs.is_empty());
    assert_eq!(first["status"]["delivery"]["state"], "blocked");
    let (_, jobs) = state
        .perform(&as_request(
            &bob,
            "receiver",
            json!({"kind":"omp_mcp","pid":std::process::id(),"activity":"unknown"}),
        ))
        .unwrap();
    assert_eq!(jobs.len(), 1);
    let state = Arc::new(Mutex::new(state));
    dispatch_jobs(&state, jobs).unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap()["message"]["id"],
        first["message_id"]
    );
}

#[test]
fn retry_recovers_never_dispatched_job_but_concurrent_claims_dispatch_once() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let receiver = ready_receiver(&mut state, &bob);
    let (first, original) = send(&mut state, &alice, &bob, "stable");
    let (retry, recovered) = send(&mut state, &alice, &bob, "stable");
    assert_eq!(retry["message_id"], first["message_id"]);
    assert_eq!(original[0].generation, recovered[0].generation);
    let state = Arc::new(Mutex::new(state));
    let other = Arc::clone(&state);
    let first = std::thread::spawn(move || dispatch_jobs(&other, original).unwrap());
    dispatch_jobs(&state, recovered).unwrap();
    first.join().unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap()["event"],
        "message"
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(state.lock().store.messages.len(), 1);
}

#[test]
fn hook_recovery_keeps_last_failure_and_private_log_pointer() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let log = directory.path().join("hook.jsonl");
    broker
        .perform(&as_request(
            &alice,
            "hook_diagnostic",
            json!({"state":"failed","detail":"session binding failed","log_path":log}),
        ))
        .unwrap();
    broker
        .perform(&as_request(
            &alice,
            "hook_diagnostic",
            json!({"state":"ready","detail":"native binding ready","log_path":log}),
        ))
        .unwrap();
    let saved = load_store(directory.path()).unwrap();
    let hook = &saved.agents[&alice.agent_id].diagnostics["hook"];
    assert_eq!(hook["state"], "ready");
    assert_eq!(hook["last_failure"]["state"], "failed");
    assert_eq!(hook["last_failure"]["detail"], "session binding failed");
    assert_eq!(hook["log_path"], json!(log));
}

#[test]
fn deactivation_preserves_receipts_and_rejects_new_delivery() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let (message, _) = send(&mut broker, &alice, &bob, "pending");
    assert!(
        broker
            .perform(&as_request(&bob, "deactivate", json!({})))
            .is_err()
    );
    broker
        .perform(&as_request(
            &bob,
            "ack",
            json!({"ids":[message["message_id"]]}),
        ))
        .unwrap();
    broker
        .perform(&as_request(&bob, "deactivate", json!({})))
        .unwrap();
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"late","idempotency_key":"late"})
            ))
            .is_err()
    );
    assert_eq!(
        send(&mut broker, &alice, &bob, "pending").0["message_id"],
        message["message_id"]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn reused_pid_is_an_exit_not_a_route_for_the_old_identity() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    broker
        .store
        .agents
        .get_mut(&bob.agent_id)
        .unwrap()
        .process_start = Some("0".to_owned());
    let agents = broker
        .perform(&as_request(&alice, "discover", json!({})))
        .unwrap()
        .0;
    assert!(agents["agents"].as_array().unwrap().is_empty());
    assert!(broker.store.agents[&bob.agent_id].exited_at.is_some());
}

#[test]
fn overlapping_subscribers_get_one_future_turn_notification_for_the_launch() {
    let (_directory, mut state) = broker();
    let alice = register(&mut state, "one");
    let bob = register(&mut state, "one");
    let older = ready_receiver(&mut state, &bob);
    let newer = ready_receiver(&mut state, &bob);
    let (_, jobs) = send(&mut state, &alice, &bob, "one notification");
    let state = Arc::new(Mutex::new(state));
    dispatch_jobs(&state, jobs).unwrap();
    assert_eq!(
        newer.recv_timeout(Duration::from_secs(1)).unwrap()["event"],
        "message"
    );
    assert!(older.try_recv().is_err());
}

#[test]
fn cursor_order_survives_clock_reversal_and_acknowledgement() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let (first, _) = send(&mut broker, &alice, &bob, "first");
    let (page, _) = broker
        .perform(&as_request(&bob, "inbox", json!({"limit":1})))
        .unwrap();
    broker
        .perform(&as_request(
            &bob,
            "ack",
            json!({"ids":[first["message_id"]]}),
        ))
        .unwrap();
    let (second, _) = send(&mut broker, &alice, &bob, "second");
    broker
        .store
        .messages
        .get_mut(second["message_id"].as_str().unwrap())
        .unwrap()
        .created_at = 0;
    let (next, _) = broker
        .perform(&as_request(
            &bob,
            "inbox",
            json!({"after":page["next_cursor"]}),
        ))
        .unwrap();
    assert_eq!(next["messages"].as_array().unwrap().len(), 1);
    assert_eq!(next["messages"][0]["id"], second["message_id"]);
    let (outbox, _) = broker
        .perform(&as_request(
            &alice,
            "outbox",
            json!({"after":first["message_id"]}),
        ))
        .unwrap();
    assert_eq!(outbox["messages"][0]["id"], second["message_id"]);
}

#[test]
fn generation_exhaustion_blocks_wake_without_reusing_or_losing_message() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    broker
        .store
        .agents
        .get_mut(&bob.agent_id)
        .unwrap()
        .generation = u64::MAX;
    let (message, jobs) = send(&mut broker, &alice, &bob, "still durable");
    assert!(jobs.is_empty());
    assert_eq!(message["status"]["delivery"]["state"], "blocked");
    assert!(
        message["status"]["delivery"]["reason"]
            .as_str()
            .unwrap()
            .contains("generation exhausted")
    );
    let (inbox, jobs) = broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    assert!(jobs.is_empty());
    assert_eq!(inbox["messages"][0]["text"], "still durable");
}

#[test]
fn rereading_an_old_page_does_not_replay_an_ambiguous_wake() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    send(&mut broker, &alice, &bob, "first page");
    let (second, _) = send(&mut broker, &alice, &bob, "unread second page");
    let (_, jobs) = broker
        .perform(&as_request(&bob, "inbox", json!({"limit":1})))
        .unwrap();
    let pending = &jobs[0];
    broker
        .outcome(
            pending,
            WakeOutcome::Unknown("queue disconnected after dispatch".into()),
        )
        .unwrap();
    let (_, jobs) = broker
        .perform(&as_request(&bob, "inbox", json!({"limit":1})))
        .unwrap();
    assert!(
        jobs.is_empty(),
        "rereading a consumed page must not retry an ambiguous wake"
    );
    let receipt = &broker.store.messages[second["message_id"].as_str().unwrap()];
    assert_eq!(receipt.delivery.generation, Some(pending.generation));
    assert_eq!(receipt.delivery.outcome.as_deref(), Some("unknown"));
    assert!(receipt.read_at.is_none());
}

#[test]
fn late_wake_failure_updates_consumed_receipt_without_changing_a_new_wake() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    let (first, jobs) = send(&mut broker, &alice, &bob, "read before queue exits");
    let old = &jobs[0];
    broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    let (_, jobs) = send(&mut broker, &alice, &bob, "new generation");
    let current = &jobs[0];
    broker
        .outcome(
            old,
            WakeOutcome::Unknown("late native queue timeout".into()),
        )
        .unwrap();
    let saved = load_store(directory.path()).unwrap();
    let receipt = &saved.messages[first["message_id"].as_str().unwrap()];
    assert_eq!(receipt.delivery.state, "consumed");
    assert_eq!(receipt.delivery.outcome.as_deref(), Some("unknown"));
    assert_eq!(
        receipt.wake_failure.as_ref().unwrap().reason,
        "late native queue timeout"
    );
    let notification = saved.agents[&bob.agent_id].notification.as_ref().unwrap();
    assert_eq!(notification.generation, current.generation);
    assert_eq!(notification.state, "pending");
}

#[test]
fn callbacks_cannot_commit_unreconciled_mutations_after_storage_recovers() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let _receiver = ready_receiver(&mut broker, &bob);
    let (committed, jobs) = send(&mut broker, &alice, &bob, "committed");
    let job = &jobs[0];
    let path = directory.path().join("state.json");
    let backup = directory.path().join("saved-state.json");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({
                    "to":bob.agent_id,"text":"uncommitted","idempotency_key":"failed commit"
                })
            ))
            .is_err()
    );
    fs::remove_dir(&path).unwrap();
    fs::rename(&backup, &path).unwrap();
    assert!(broker.outcome(job, WakeOutcome::Accepted).is_err());
    let saved = load_store(directory.path()).unwrap();
    assert!(
        !saved
            .messages
            .values()
            .any(|m| m.text.as_deref() == Some("uncommitted"))
    );
    assert_eq!(
        saved.messages[committed["message_id"].as_str().unwrap()]
            .delivery
            .outcome
            .as_deref(),
        Some("pending")
    );
}

#[test]
fn fingerprints_distinguish_all_four_scopes_and_preserve_legacy_receipts() {
    let digests: std::collections::BTreeSet<_> = [
        Scope::Workspace,
        Scope::Session,
        Scope::Repository,
        Scope::All,
    ]
    .into_iter()
    .map(|scope| {
        payload_digest(
            "recipient",
            "same text",
            &MessageScope {
                scope,
                fingerprint_version: 1,
            },
        )
    })
    .collect();
    assert_eq!(digests.len(), 4);

    for (all, scope) in [(false, Scope::Repository), (true, Scope::All)] {
        let (directory, mut broker) = broker();
        let alice = register(&mut broker, "one");
        let bob = register(&mut broker, "one");
        let params = json!({"to":bob.agent_id,"text":"legacy body","idempotency_key":"legacy key","scope":scope});
        let (first, _) = broker
            .perform(&as_request(&alice, "send", params.clone()))
            .unwrap();
        let id = first["message_id"].as_str().unwrap();
        broker
            .perform(&as_request(&bob, "ack", json!({"ids":[id]})))
            .unwrap();
        let mut legacy = serde_json::to_value(&broker.store).unwrap();
        for agent in legacy["agents"].as_object_mut().unwrap().values_mut() {
            let repository = agent.as_object_mut().unwrap().remove("repository").unwrap();
            agent["workspace"] = repository;
        }
        let message = legacy["messages"][id].as_object_mut().unwrap();
        message.remove("scope");
        message.remove("fingerprint_version");
        message.insert("all_scope".into(), json!(all));
        message.insert(
            "payload_digest".into(),
            json!(payload_digest(
                &bob.agent_id,
                "legacy body",
                &MessageScope {
                    scope,
                    fingerprint_version: 0
                },
            )),
        );
        broker.store = serde_json::from_value(legacy).unwrap();
        broker.maintain(now()).unwrap();
        let (retry, jobs) = broker
            .perform(&as_request(&alice, "send", params.clone()))
            .unwrap();
        assert_eq!(retry["message_id"], first["message_id"]);
        assert_eq!(retry["status"]["scope"], json!(scope));
        assert_eq!(retry["status"]["state"], "processed");
        assert!(jobs.is_empty());
        for changed_scope in [
            Scope::Workspace,
            Scope::Session,
            Scope::Repository,
            Scope::All,
        ] {
            if changed_scope == scope {
                continue;
            }
            let mut changed = params.clone();
            changed["scope"] = json!(changed_scope);
            assert!(
                broker
                    .perform(&as_request(&alice, "send", changed))
                    .is_err()
            );
        }
        let saved = load_store(directory.path()).unwrap();
        assert!(saved.messages[id].text.is_none());
        assert_eq!(saved.messages[id].scope.scope, scope);
        let serialized = serde_json::to_value(saved).unwrap();
        assert!(serialized["messages"][id].get("all_scope").is_none());
        assert!(
            serialized["agents"][&alice.agent_id]
                .get("workspace")
                .is_none()
        );
        assert_eq!(
            serialized["agents"][&alice.agent_id]["repository"],
            alice.params["repository"]
        );
    }
}

fn discovered_ids(broker: &mut Broker, caller: &Request, scope: Option<&str>) -> Vec<String> {
    let params = scope
        .map(|scope| json!({"scope":scope}))
        .unwrap_or(json!({}));
    let (value, _) = broker
        .perform(&as_request(caller, "discover", params))
        .unwrap();
    assert_eq!(value["scope"], scope.unwrap_or("workspace"));
    let mut ids: Vec<_> = value["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[test]
fn live_workspace_scope_is_not_repository_session_or_receiver_readiness() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let carol = register(&mut broker, "one");
    let dave = register(&mut broker, "one");
    let eve = register(&mut broker, "different-repository");
    let orphan = register(&mut broker, "one");
    let fixture = &directory.context;
    let workspace = alice.params["repository"].as_str().unwrap().to_owned();
    {
        let mut panes = fixture.panes.borrow_mut();
        panes.get_mut(&carol.agent_id).unwrap().workspace_id = "another-workspace".into();
        panes.get_mut(&eve.agent_id).unwrap().workspace_id = workspace.clone();
        panes.remove(&dave.agent_id);
        panes.remove(&orphan.agent_id);
    }
    fixture.publish();
    let other_session = Session::new_in(fixture.session.root(), "another session").unwrap();
    other_session
        .publish(
            1,
            vec![ContextWorkspace {
                id: workspace.clone(),
                name: "same id, different session".into(),
            }],
            vec![ContextPane {
                pane_id: "terminal_1".into(),
                workspace_id: workspace,
                pane_name: "another session pane".into(),
                agent_id: Some(dave.agent_id.clone()),
            }],
        )
        .unwrap();
    broker
        .store
        .agents
        .get_mut(&orphan.agent_id)
        .unwrap()
        .harness = "codex".into();
    broker
        .store
        .agents
        .get_mut(&orphan.agent_id)
        .unwrap()
        .session_id = Some("native-thread-not-bkpr-session".into());
    broker
        .perform(&as_request(
            &orphan,
            "lease",
            json!({"pid":std::process::id()}),
        ))
        .unwrap();
    let mut expected = vec![bob.agent_id.clone(), eve.agent_id.clone()];
    expected.sort();
    assert_eq!(discovered_ids(&mut broker, &alice, None), expected);
    expected.push(carol.agent_id.clone());
    expected.sort();
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("session")),
        expected
    );
    let mut repository_peers = vec![
        bob.agent_id.clone(),
        carol.agent_id.clone(),
        dave.agent_id.clone(),
        orphan.agent_id.clone(),
    ];
    repository_peers.sort();
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("repository")),
        repository_peers
    );
    repository_peers.push(eve.agent_id.clone());
    repository_peers.sort();
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("all")),
        repository_peers
    );
    let (local, _) = broker
        .perform(&as_request(&alice, "discover", json!({})))
        .unwrap();
    let starting = local["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == bob.agent_id)
        .unwrap();
    assert_eq!(starting["attached"], true);
    assert_eq!(starting["readiness"]["can_notify"], false);
    let (detached, _) = broker
        .perform(&as_request(&orphan, "whoami", json!({})))
        .unwrap();
    assert_eq!(detached["active"], true);
    assert_eq!(detached["attached"], false);
    assert!(detached["context"].is_null());
    assert_eq!(detached["session_id"], "native-thread-not-bkpr-session");
    for scope in ["workspace", "session"] {
        assert!(
            broker
                .perform(&as_request(&orphan, "discover", json!({"scope":scope})))
                .is_err()
        );
        assert!(broker.perform(&as_request(&alice, "send",
            json!({"to":orphan.agent_id,"text":"not a member","idempotency_key":scope,"scope":scope}))).is_err());
    }
    for (recipient, scope) in [
        (&carol, "session"),
        (&dave, "repository"),
        (&orphan, "repository"),
        (&eve, "workspace"),
    ] {
        if recipient.agent_id != eve.agent_id {
            assert!(broker.perform(&as_request(&alice, "send", json!({
                "to":recipient.agent_id,"text":"default denied","idempotency_key":recipient.agent_id
            }))).is_err());
        }
        broker.perform(&as_request(&alice, "send", json!({
            "to":recipient.agent_id,"text":"explicit member","idempotency_key":scope.to_owned()+&recipient.agent_id,"scope":scope
        }))).unwrap();
    }
    assert!(broker.perform(&as_request(&alice, "send", json!({
        "to":eve.agent_id,"text":"wrong repository","idempotency_key":"wrong repository","scope":"repository"
    }))).is_err());
    assert!(broker.perform(&as_request(&alice, "send", json!({
        "to":dave.agent_id,"text":"wrong session","idempotency_key":"wrong session","scope":"session"
    }))).is_err());
    broker.perform(&as_request(&alice, "send", json!({
        "to":eve.agent_id,"text":"all explicit","idempotency_key":"all explicit","scope":"all"
    }))).unwrap();
}

#[test]
fn missing_caller_context_fails_closed_without_repository_fallback() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    directory.context.panes.borrow_mut().remove(&alice.agent_id);
    directory.context.publish();
    for params in [
        json!({}),
        json!({"scope":"workspace"}),
        json!({"scope":"session"}),
    ] {
        let error = broker
            .perform(&as_request(&alice, "discover", params.clone()))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("current Bkpr pane context is unavailable"));
        let mut send_params = params;
        send_params["to"] = json!(bob.agent_id);
        send_params["text"] = json!("must not send");
        send_params["idempotency_key"] = json!("missing context");
        assert!(
            broker
                .perform(&as_request(&alice, "send", send_params))
                .is_err()
        );
    }
    assert!(broker.store.messages.is_empty());
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("repository")),
        vec![bob.agent_id.clone()]
    );
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("all")),
        vec![bob.agent_id.clone()]
    );
    broker.perform(&as_request(&alice, "send",
        json!({"to":bob.agent_id,"text":"explicit","idempotency_key":"explicit","scope":"repository"}))).unwrap();
    let (whoami, _) = broker
        .perform(&as_request(&alice, "whoami", json!({})))
        .unwrap();
    assert_eq!(whoami["attached"], false);
    assert!(whoami["context"].is_null());
}

#[test]
fn moves_rename_and_close_use_latest_publication_without_losing_replays() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let (first, _) = send(&mut broker, &alice, &bob, "before move");
    let workspace = alice.params["repository"].as_str().unwrap().to_owned();
    let fixture = &directory.context;
    fixture
        .panes
        .borrow_mut()
        .get_mut(&bob.agent_id)
        .unwrap()
        .workspace_id = "moved".into();
    fixture.publish_as(
        vec![
            ContextWorkspace {
                id: workspace.clone(),
                name: "original".into(),
            },
            ContextWorkspace {
                id: "moved".into(),
                name: "renamed workspace".into(),
            },
        ],
        fixture.panes.borrow().values().cloned().collect(),
    );
    assert!(discovered_ids(&mut broker, &alice, None).is_empty());
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("session")),
        vec![bob.agent_id.clone()]
    );
    let (whoami, _) = broker
        .perform(&as_request(&bob, "whoami", json!({})))
        .unwrap();
    assert_eq!(whoami["context"]["workspace_id"], "moved");
    assert_eq!(whoami["context"]["workspace_name"], "renamed workspace");
    assert_eq!(
        whoami["context"]["session_id"],
        fixture.session.session_id()
    );
    assert!(
        broker
            .perform(&as_request(
                &alice,
                "send",
                json!({"to":bob.agent_id,"text":"after move","idempotency_key":"after move"})
            ))
            .is_err()
    );
    broker.perform(&as_request(&alice, "send",
        json!({"to":bob.agent_id,"text":"after move","idempotency_key":"after move","scope":"session"}))).unwrap();
    fixture.panes.borrow_mut().remove(&bob.agent_id);
    fixture.publish();
    assert!(discovered_ids(&mut broker, &alice, Some("session")).is_empty());
    let snapshot = snapshot_status(&broker.store);
    let closed = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == bob.agent_id)
        .unwrap();
    assert_eq!(closed["attached"], false);
    assert!(closed["context"].is_null());
    let (retry, jobs) = send(&mut broker, &alice, &bob, "before move");
    assert_eq!(retry["message_id"], first["message_id"]);
    assert_eq!(retry["duplicate"], true);
    assert!(jobs.is_empty());
    let (inbox, _) = broker
        .perform(&as_request(&bob, "inbox", json!({})))
        .unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
    broker
        .perform(&as_request(
            &bob,
            "ack",
            json!({"ids":[first["message_id"]]}),
        ))
        .unwrap();
    let (retry, _) = send(&mut broker, &alice, &bob, "before move");
    assert_eq!(retry["status"]["state"], "processed");
    assert_eq!(
        discovered_ids(&mut broker, &alice, Some("repository")),
        vec![bob.agent_id.clone()]
    );
}

#[test]
fn each_selected_scope_survives_ack_reload_and_rejects_scope_reuse() {
    for selected in [
        Scope::Workspace,
        Scope::Session,
        Scope::Repository,
        Scope::All,
    ] {
        let (directory, mut broker) = broker();
        let alice = register(&mut broker, "one");
        let bob = register(&mut broker, "one");
        let params = json!({"to":bob.agent_id,"text":"same payload","idempotency_key":"scope key","scope":selected});
        let (first, _) = broker
            .perform(&as_request(&alice, "send", params.clone()))
            .unwrap();
        assert_eq!(first["status"]["scope"], json!(selected));
        broker
            .perform(&as_request(
                &bob,
                "ack",
                json!({"ids":[first["message_id"]]}),
            ))
            .unwrap();
        broker.store = load_store(directory.path()).unwrap();
        directory.context.panes.borrow_mut().clear();
        directory.context.publish();
        let (retry, jobs) = broker
            .perform(&as_request(&alice, "send", params.clone()))
            .unwrap();
        assert_eq!(retry["message_id"], first["message_id"]);
        assert_eq!(retry["status"]["state"], "processed");
        assert!(jobs.is_empty());
        for other in [
            Scope::Workspace,
            Scope::Session,
            Scope::Repository,
            Scope::All,
        ] {
            if other == selected {
                continue;
            }
            let mut changed = params.clone();
            changed["scope"] = json!(other);
            assert!(
                broker
                    .perform(&as_request(&alice, "send", changed))
                    .is_err()
            );
        }
        let mut changed = params;
        changed["text"] = json!("different payload");
        assert!(
            broker
                .perform(&as_request(&alice, "send", changed))
                .is_err()
        );
        assert_eq!(broker.store.messages.len(), 1);
    }
}
