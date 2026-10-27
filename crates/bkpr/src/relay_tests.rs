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
    register_harness(broker, workspace, "omp")
}

fn register_harness(broker: &mut Broker, workspace: &str, harness: &str) -> Request {
    let workspace = broker.directory.join(workspace);
    fs::create_dir_all(&workspace).unwrap();
    let request = Request {
        protocol: PROTOCOL_VERSION,
        agent_id: random_hex(16).unwrap(),
        token: random_hex(32).unwrap(),
        method: "register".into(),
        params: json!({"cwd":workspace,"repository":workspace,"harness":harness,"pid":std::process::id()}),
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
            status: None,
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
            binding: None,
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
            json!({"kind":"binding","state":"failed","detail":"native transport failed","log_path":log}),
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
    let binding = &saved.agents[&alice.agent_id].diagnostics["binding"];
    assert_eq!(binding["state"], "failed");
    assert_eq!(binding["detail"], "native transport failed");
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
                status: None,
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

fn discovered(broker: &mut Broker, caller: &Request, target: &Request) -> Value {
    let (value, _) = broker
        .perform(&as_request(caller, "discover", json!({})))
        .unwrap();
    value["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == target.agent_id.as_str())
        .unwrap()
        .clone()
}

#[test]
fn explicit_titles_and_live_pane_names_are_reported_separately() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let record = discovered(&mut broker, &alice, &bob);
    assert!(record["title"].is_null());
    assert_eq!(
        record["context"]["pane_name"],
        format!("{} pane", bob.agent_id)
    );

    broker
        .perform(&as_request(&bob, "set_title", json!({"title":"reviewer"})))
        .unwrap();
    let context = fixture(&broker);
    context
        .panes
        .borrow_mut()
        .get_mut(&bob.agent_id)
        .unwrap()
        .pane_name = "Fix flaky lease test".into();
    context.publish();
    let record = discovered(&mut broker, &alice, &bob);
    assert_eq!(record["title"], "reviewer");
    assert_eq!(record["context"]["pane_name"], "Fix flaky lease test");
}

#[test]
fn default_discovery_view_is_compact_and_detail_keeps_full_records() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    broker
        .perform(&as_request(&bob, "set_title", json!({"title":"reviewer"})))
        .unwrap();
    let (full, _) = broker
        .perform(&as_request(&alice, "discover", json!({})))
        .unwrap();

    let compact = discovery_view(full.clone(), false);
    assert_eq!(compact["scope"], "workspace");
    let summary = &compact["agents"][0];
    let keys: Vec<_> = summary.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "attached",
            "can_notify",
            "harness",
            "id",
            "pane_id",
            "pane_name",
            "project",
            "session",
            "title",
            "workspace"
        ]
    );
    assert_eq!(summary["id"], bob.agent_id.as_str());
    assert_eq!(summary["title"], "reviewer");
    assert_eq!(summary["pane_name"], format!("{} pane", bob.agent_id));
    assert_eq!(summary["attached"], true);
    assert_eq!(summary["can_notify"], false);
    assert_eq!(discovery_view(full.clone(), true), full);
}

#[test]
fn discovery_detail_is_mcp_only_and_strictly_boolean() {
    let mut params = json!({"scope":"session","detail":true});
    assert_eq!(
        discovery_detail("discover", &mut params).unwrap(),
        Some(true)
    );
    assert_eq!(params, json!({"scope":"session"}));
    assert_eq!(
        discovery_detail("discover", &mut json!({})).unwrap(),
        Some(false)
    );
    assert!(discovery_detail("discover", &mut json!({"detail":"yes"})).is_err());
    let mut other = json!({"detail":true});
    assert_eq!(discovery_detail("whoami", &mut other).unwrap(), None);
    assert_eq!(other, json!({"detail":true}));
}

// LEGACY_COMPAT(next-major): stores and live brokers from earlier releases
// report the shared `"{harness} agent"` default title.
#[test]
fn legacy_default_titles_are_reported_as_unset() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    broker.store.agents.get_mut(&bob.agent_id).unwrap().title = Some("omp agent".into());
    assert!(discovered(&mut broker, &alice, &bob)["title"].is_null());

    let old_broker = json!({"agents":[{"id":"old","harness":"codex","title":"codex agent"}]});
    assert_eq!(
        discovery_view(old_broker, false)["agents"][0],
        json!({"id":"old","harness":"codex"})
    );
}

#[test]
fn recovery_agents_is_read_only() {
    let (directory, mut broker) = broker();
    let alice = register(&mut broker, "alice");
    let state = directory.path().join("state.json");
    let before = fs::read(&state).unwrap();
    let mtime = fs::metadata(&state).unwrap().modified().unwrap();

    let agents = recovery_agents_in(directory.path()).unwrap();

    let found = &agents.by_id[&alice.agent_id];
    assert_eq!(found.harness, "omp");
    assert_eq!(
        found.live,
        cfg!(target_os = "linux"),
        "recovery liveness requires a verified process birth identity"
    );
    assert_eq!(fs::read(&state).unwrap(), before);
    assert_eq!(fs::metadata(&state).unwrap().modified().unwrap(), mtime);
    assert!(!directory.path().join("broker.sock").exists());
    assert!(!directory.path().join("relay").exists());
}

// Verified process birth identities currently require Linux procfs.
#[cfg(target_os = "linux")]
#[test]
fn recovery_agents_liveness_and_holders() {
    let (directory, mut broker) = broker();
    let live = register(&mut broker, "live");
    let exited = register(&mut broker, "exited");
    let unstamped = register(&mut broker, "unstamped");
    let recycled = register(&mut broker, "recycled");
    for (request, session) in [
        (&live, "s"),
        (&exited, "s"),
        (&unstamped, "u"),
        (&recycled, "r"),
    ] {
        broker
            .store
            .agents
            .get_mut(&request.agent_id)
            .unwrap()
            .session_id = Some(session.into());
    }
    broker
        .store
        .agents
        .get_mut(&exited.agent_id)
        .unwrap()
        .exited_at = Some(1);
    broker
        .store
        .agents
        .get_mut(&unstamped.agent_id)
        .unwrap()
        .process_start = None;
    broker
        .store
        .agents
        .get_mut(&recycled.agent_id)
        .unwrap()
        .process_start = Some("1".into());
    fs::write(
        directory.path().join("state.json"),
        serde_json::to_vec(&broker.store).unwrap(),
    )
    .unwrap();

    let agents = recovery_agents_in(directory.path()).unwrap();

    assert!(agents.by_id[&live.agent_id].live);
    assert!(!agents.by_id[&exited.agent_id].live);
    assert!(!agents.by_id[&unstamped.agent_id].live);
    assert!(!agents.by_id[&recycled.agent_id].live);
    assert_eq!(agents.live_holders["s"], vec![live.agent_id.clone()]);
    assert!(!agents.live_holders.contains_key("u"));
    assert!(!agents.live_holders.contains_key("r"));
}

/// Start-if-absent never replaces a live broker: a listening socket is left alone.
#[test]
fn ensure_broker_keeps_a_live_broker() {
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("broker.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let identity = fs::metadata(&socket).unwrap().ino();
    assert_eq!(ensure_broker_in(runtime.path()).unwrap(), socket);
    assert_eq!(fs::metadata(&socket).unwrap().ino(), identity);
    listener.set_nonblocking(true).unwrap();
    // The probe connection reached the original listener.
    assert!(listener.accept().is_ok());
}

#[cfg(target_os = "linux")]
/// A broker started from a pane must outlive it: its own session and process group (so
/// Ctrl+C at that pane's prompt or the pane's hangup never reach it), with default signal
/// dispositions even when the starter ignores them.
#[test]
fn broker_spawn_is_detached_with_default_signals() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("report");
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "{{ cut -d' ' -f5,6 /proc/self/stat; grep SigIgn /proc/self/status; echo $$; }} > {}",
        report.display()
    ));
    // SAFETY: SIG_IGN/SIG_DFL install no handler; SIGHUP is unused by the test harness.
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mut child = spawn_detached(&mut command).unwrap();
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_DFL) };
    assert!(child.wait().unwrap().success());
    let text = fs::read_to_string(&report).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let pid: u32 = lines[2].trim().parse().unwrap();
    // `cut` is a child of the shell: its pgrp and session are the shell's own pid.
    assert_eq!(lines[0], format!("{pid} {pid}"), "{text}");
    let ignored = u64::from_str_radix(lines[1].trim_start_matches("SigIgn:").trim(), 16).unwrap();
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
        assert_eq!(
            ignored & (1 << (signal - 1)),
            0,
            "signal {signal} ignored: {text}"
        );
    }
}

#[test]
fn compact_discovery_reports_live_harness_status() {
    let (_directory, mut broker) = broker();
    let alice = register(&mut broker, "one");
    let bob = register(&mut broker, "one");
    let context = fixture(&broker);
    context
        .panes
        .borrow_mut()
        .get_mut(&bob.agent_id)
        .unwrap()
        .status = Some(bkpr::context_contract::PaneStatus {
        state: bkpr::Status::Blocked,
        attention: Some(bkpr::Attention::Question),
        message: Some("Which branch?".into()),
    });
    context.publish();
    let full = discovered(&mut broker, &alice, &bob);
    assert_eq!(
        full["context"]["status"],
        json!({"state":"blocked","attention":"question","message":"Which branch?"})
    );
    let summary = agent_summary(&full);
    assert_eq!(summary["status"], "blocked");
    assert_eq!(summary["attention"], "question");
    assert_eq!(summary["status_message"], "Which branch?");
    assert!(
        agent_summary(&discovered(&mut broker, &bob, &alice))
            .get("status")
            .is_none()
    );
}

const AGY_ROOT: &str = "81121061-d248-4d4a-b8b0-a5b711d08490";
const AGY_OTHER_ROOT: &str = "fce9f077-6258-41b0-9ad3-e30b561120e4";

fn agy_receiver_params(root: &str) -> Value {
    json!({"kind":"agy_native","session_id":root,"pid":std::process::id()})
}

fn ready_agy(broker: &mut Broker, agent: &Request) -> mpsc::Receiver<Value> {
    broker
        .perform(&as_request(
            agent,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    broker
        .perform(&as_request(
            agent,
            "receiver",
            agy_receiver_params(AGY_ROOT),
        ))
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let binding = agy_receiver_binding(
        &broker.store.agents[&agent.agent_id],
        &agy_receiver_params(AGY_ROOT),
    )
    .unwrap();
    broker
        .subscribers
        .entry(agent.agent_id.clone())
        .or_default()
        .push(Subscriber {
            id: "native-agy-test".to_owned(),
            sender,
            binding: Some(binding),
        });
    receiver
}

fn agy_callback(agent: &Request, job: &WakeJob, outcome: &str, reason: Option<&str>) -> Request {
    let mut params = agy_receiver_params(AGY_ROOT);
    params["notification_id"] = json!(job.id);
    params["generation"] = json!(job.generation);
    params["outcome"] = json!(outcome);
    if let Some(reason) = reason {
        params["reason"] = json!(reason);
    }
    as_request(agent, "agy_wake_outcome", params)
}

#[test]
fn agy_metadata_requires_canonical_root_and_ignores_child_conversation_ids() {
    let call = json!({"params":{"_meta":{
        "antigravity.google/root_conversation_id":AGY_ROOT,
        "antigravity.google/conversation_id":AGY_OTHER_ROOT,
    }}});
    assert_eq!(agy_root_metadata(&call).unwrap(), AGY_ROOT);
    for metadata in [
        json!({}),
        json!(null),
        json!({"antigravity.google/conversation_id":AGY_ROOT}),
        json!({"antigravity.google/root_conversation_id":42}),
        json!({"antigravity.google/root_conversation_id":""}),
        json!({"antigravity.google/root_conversation_id":"81121061-D248-4D4A-B8B0-A5B711D08490"}),
        json!({"antigravity.google/root_conversation_id":"00000000-0000-0000-0000-000000000000"}),
        json!({"antigravity.google/root_conversation_id":format!("{AGY_ROOT}\n")}),
        json!({"antigravity.google/root_conversation_id":AGY_ROOT.replace('-', "")}),
    ] {
        assert!(agy_root_metadata(&json!({"params":{"_meta":metadata}})).is_err());
    }
    let instructions = agy_bootstrap_instructions(Path::new("/private/bkpr's binary")).unwrap();
    let command = crate::claude::shell_quote("/private/bkpr's binary") + " agy-receiver";
    assert!(instructions.contains(&serde_json::to_string(&command).unwrap()));
    assert!(instructions.contains("IsDaemon:true"));
    assert!(!instructions.contains("ANTIGRAVITY_CSRF_TOKEN="));
}

#[test]
fn agy_admission_requires_mcp_root_before_native_receiver_and_keeps_status_unknown() {
    let (_directory, mut broker) = broker();
    let agy = register_harness(&mut broker, "one", "agy");
    assert!(
        broker
            .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
            .is_err()
    );
    assert!(
        broker
            .perform(&as_request(
                &agy,
                "bind_session",
                json!({"session_id":"not-a-uuid"})
            ))
            .is_err()
    );
    broker
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    assert!(
        broker
            .perform(&as_request(
                &agy,
                "receiver",
                agy_receiver_params(AGY_OTHER_ROOT)
            ))
            .is_err()
    );
    for field in ["queue_endpoint", "codex_program"] {
        let mut params = json!({"session_id":AGY_ROOT});
        params[field] = json!("secret must not be persisted");
        assert!(
            broker
                .perform(&as_request(&agy, "bind_session", params))
                .is_err()
        );
    }
    for patch in [
        json!({"kind":"omp_mcp"}),
        json!({"activity":"busy"}),
        json!({"diagnostic":"native-secret"}),
        json!({"diagnostic_log":"/private/native-secret"}),
        json!({"lease_ms":0}),
        json!({"lease_ms":300001}),
    ] {
        let mut params = agy_receiver_params(AGY_ROOT);
        params
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(
            broker
                .perform(&as_request(&agy, "receiver", params))
                .is_err()
        );
    }
    let _receiver = ready_agy(&mut broker, &agy);
    let (who, _) = broker
        .perform(&as_request(&agy, "whoami", json!({})))
        .unwrap();
    assert_eq!(who["session_id"], AGY_ROOT);
    assert_eq!(who["readiness"]["receiver"]["kind"], "agy_native");
    assert_eq!(who["readiness"]["receiver"]["activity"], "unknown");
    assert_eq!(
        who["readiness"]["future_turn_transport"],
        "private_agy_when_idle"
    );
    assert_eq!(who["readiness"]["can_notify"], true);
    assert!(who["context"]["status"].is_null());
    let snapshot = serde_json::to_string(&broker.store).unwrap();
    assert!(!snapshot.contains("native-secret"));
    assert!(!snapshot.contains("ANTIGRAVITY_LS_ADDRESS"));
}

#[test]
fn agy_unix_pointer_write_is_not_native_acceptance_and_coalesces_until_read_and_ack() {
    let (_directory, mut state) = broker();
    let sender = register(&mut state, "one");
    let agy = register_harness(&mut state, "one", "agy");
    state
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    state
        .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
        .unwrap();
    let (first, jobs) = send(&mut state, &sender, &agy, "first private body");
    assert!(jobs.is_empty());
    let state = Arc::new(Mutex::new(state));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let server_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(
        &mut client,
        &json!({
            "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
            "method":"subscribe","params":agy_receiver_params(AGY_ROOT),
        }),
    )
    .unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap()["event"], "ready");
    let event = read_frame(&mut reader).unwrap().unwrap();
    assert_eq!(event["message"]["id"], first["message_id"]);
    assert!(!event.to_string().contains("private body"));
    let job = WakeJob {
        recipient: agy.agent_id.clone(),
        id: first["message_id"].as_str().unwrap().to_owned(),
        generation: event["generation"].as_u64().unwrap(),
    };
    let (second, jobs) = send(&mut state.lock(), &sender, &agy, "second private body");
    assert!(jobs.is_empty());
    {
        let mut state = state.lock();
        assert_eq!(
            state.store.messages[&job.id].delivery.outcome.as_deref(),
            Some("pending")
        );
        let mut validate = agy_callback(&agy, &job, "accepted", None);
        validate.method = "agy_wake_validate".into();
        assert_eq!(state.perform(&validate).unwrap().0["current"], true);
        state
            .perform(&agy_callback(&agy, &job, "accepted", None))
            .unwrap();
        let first_message = &state.store.messages[&job.id];
        assert_eq!(first_message.delivery.state, "accepted");
        assert!(first_message.read_at.is_none());
        assert!(first_message.acknowledged_at.is_none());
        let second_message = &state.store.messages[second["message_id"].as_str().unwrap()];
        assert_eq!(second_message.delivery.state, "coalesced");
        assert_eq!(second_message.delivery.outcome.as_deref(), Some("accepted"));
        let (inbox, _) = state
            .perform(&as_request(&agy, "inbox", json!({})))
            .unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
        assert!(state.store.messages[&job.id].read_at.is_some());
        assert!(state.store.messages[&job.id].acknowledged_at.is_none());
        state
            .perform(&as_request(
                &agy,
                "ack",
                json!({"ids":[job.id,second["message_id"]]}),
            ))
            .unwrap();
        assert!(
            state.store.messages[first["message_id"].as_str().unwrap()]
                .text
                .is_none()
        );
    }
    drop(reader);
    worker.join().unwrap();
}

#[test]
fn agy_rebind_invalidates_receiver_and_uncertain_generation_without_replaying() {
    let (_directory, mut broker) = broker();
    let sender = register(&mut broker, "one");
    let agy = register_harness(&mut broker, "one", "agy");
    let receiver = ready_agy(&mut broker, &agy);
    let (first, jobs) = send(&mut broker, &sender, &agy, "old root pointer");
    let job = jobs[0].clone();
    let broker = Arc::new(Mutex::new(broker));
    dispatch_jobs(&broker, jobs).unwrap();
    assert_eq!(receiver.recv().unwrap()["event"], "message");
    let mut broker = broker.lock();
    broker
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_OTHER_ROOT}),
        ))
        .unwrap();
    assert_eq!(receiver.recv().unwrap()["event"], "invalidated");
    let current = &broker.store.agents[&agy.agent_id];
    assert_eq!(readiness(current, 0)["receiver_alive"], false);
    assert_eq!(readiness(current, 0)["can_notify"], false);
    assert_eq!(current.notification.as_ref().unwrap().state, "unknown");
    assert!(
        broker
            .perform(&agy_callback(&agy, &job, "accepted", None))
            .is_err()
    );
    assert!(
        broker
            .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
            .is_err()
    );
    let (_, jobs) = broker
        .perform(&as_request(
            &agy,
            "receiver",
            agy_receiver_params(AGY_OTHER_ROOT),
        ))
        .unwrap();
    assert!(
        jobs.is_empty(),
        "uncertain old-root wake must not be replayed against a new root"
    );
    assert_eq!(
        broker.store.messages[first["message_id"].as_str().unwrap()]
            .wake_failure
            .as_ref()
            .unwrap()
            .state,
        "unknown"
    );
}

#[test]
fn agy_authenticated_callbacks_preserve_old_read_receipts_and_never_change_new_generation() {
    let (directory, mut broker) = broker();
    let sender = register(&mut broker, "one");
    let agy = register_harness(&mut broker, "one", "agy");
    let _receiver = ready_agy(&mut broker, &agy);
    let (first, jobs) = send(&mut broker, &sender, &agy, "first");
    let old = &jobs[0];
    broker
        .store
        .agents
        .get_mut(&agy.agent_id)
        .unwrap()
        .notification
        .as_mut()
        .unwrap()
        .dispatched = true;
    broker
        .perform(&as_request(&agy, "inbox", json!({})))
        .unwrap();
    let (_, jobs) = send(&mut broker, &sender, &agy, "new");
    let current = &jobs[0];
    broker
        .perform(&agy_callback(
            &agy,
            old,
            "unknown",
            Some("transport_unknown"),
        ))
        .unwrap();
    let saved = load_store(directory.path()).unwrap();
    let old_receipt = &saved.messages[first["message_id"].as_str().unwrap()];
    assert_eq!(old_receipt.delivery.state, "consumed");
    assert_eq!(old_receipt.delivery.outcome.as_deref(), Some("unknown"));
    assert_eq!(
        old_receipt.wake_failure.as_ref().unwrap().generation,
        old.generation
    );
    assert_eq!(
        saved.agents[&agy.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .generation,
        current.generation
    );
    assert_eq!(
        saved.agents[&agy.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .state,
        "pending"
    );
    for mut callback in [
        Request {
            token: "0".repeat(64),
            ..agy_callback(&agy, current, "accepted", None)
        },
        agy_callback(
            &agy,
            &WakeJob {
                generation: current.generation + 1,
                ..current.clone()
            },
            "accepted",
            None,
        ),
        agy_callback(&agy, current, "unknown", Some("raw-CSRF-secret")),
    ] {
        assert!(broker.perform(&callback).is_err());
        callback.params["pid"] = json!(0);
        assert!(broker.perform(&callback).is_err());
    }
    broker.storage_failure = Some("injected durable failure".into());
    assert!(
        broker
            .perform(&agy_callback(&agy, current, "accepted", None))
            .is_err()
    );
    let mut validate = agy_callback(&agy, current, "accepted", None);
    validate.method = "agy_wake_validate".into();
    assert!(broker.perform(&validate).is_err());
    assert_eq!(
        broker.store.agents[&agy.agent_id]
            .notification
            .as_ref()
            .unwrap()
            .state,
        "pending"
    );
}

#[test]
fn agy_unknown_and_failed_native_outcomes_are_not_blindly_resent() {
    for (outcome, reason) in [
        ("failed", "connect_failed"),
        ("failed", "http_rejected"),
        ("unknown", "transport_unknown"),
        ("unknown", "invalid_response"),
    ] {
        let (_directory, mut broker) = broker();
        let sender = register(&mut broker, "one");
        let agy = register_harness(&mut broker, "one", "agy");
        let _receiver = ready_agy(&mut broker, &agy);
        let (_, jobs) = send(&mut broker, &sender, &agy, "first");
        let job = &jobs[0];
        broker
            .store
            .agents
            .get_mut(&agy.agent_id)
            .unwrap()
            .notification
            .as_mut()
            .unwrap()
            .dispatched = true;
        broker
            .perform(&agy_callback(&agy, job, outcome, Some(reason)))
            .unwrap();
        assert!(send(&mut broker, &sender, &agy, "second").1.is_empty());
        assert!(
            broker
                .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
                .unwrap()
                .1
                .is_empty()
        );
        assert_eq!(
            broker.store.agents[&agy.agent_id]
                .notification
                .as_ref()
                .unwrap()
                .state,
            outcome
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn agy_receiver_birth_mismatch_and_launch_exit_invalidate_authentication() {
    let (_directory, mut broker) = broker();
    let agy = register_harness(&mut broker, "one", "agy");
    let _receiver = ready_agy(&mut broker, &agy);
    broker
        .store
        .agents
        .get_mut(&agy.agent_id)
        .unwrap()
        .receiver
        .process_start = Some("recycled-pid".into());
    assert!(!receiver_alive(
        &broker.store.agents[&agy.agent_id].receiver
    ));
    assert!(
        agy_receiver_binding(
            &broker.store.agents[&agy.agent_id],
            &agy_receiver_params(AGY_ROOT)
        )
        .is_err()
    );
    broker
        .store
        .agents
        .get_mut(&agy.agent_id)
        .unwrap()
        .process_start = Some("exited-launch".into());
    assert!(
        broker
            .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
            .is_err()
    );
    assert!(broker.store.agents[&agy.agent_id].exited_at.is_some());
}

#[test]
fn agy_subscription_requires_exact_native_binding_and_durable_store_health() {
    for rejected in [
        json!({}),
        json!({"kind":"agy_native","session_id":AGY_OTHER_ROOT,"pid":std::process::id()}),
        json!({"kind":"agy_native","session_id":AGY_ROOT,"pid":0}),
    ] {
        let (_directory, mut state) = broker();
        let agy = register_harness(&mut state, "one", "agy");
        state
            .perform(&as_request(
                &agy,
                "bind_session",
                json!({"session_id":AGY_ROOT}),
            ))
            .unwrap();
        state
            .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
            .unwrap();
        let state = Arc::new(Mutex::new(state));
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let server_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
        write_frame(
            &mut client,
            &json!({
                "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
                "method":"subscribe","params":rejected,
            }),
        )
        .unwrap();
        assert!(
            read_frame(&mut BufReader::new(client))
                .unwrap()
                .unwrap()
                .get("error")
                .is_some()
        );
        worker.join().unwrap();
        assert!(state.lock().subscribers.is_empty());
    }
    let (_directory, mut state) = broker();
    let agy = register_harness(&mut state, "one", "agy");
    let _receiver = ready_agy(&mut state, &agy);
    state.storage_failure = Some("injected durable failure".into());
    let state = Arc::new(Mutex::new(state));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let server_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(
        &mut client,
        &json!({
            "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
            "method":"subscribe","params":agy_receiver_params(AGY_ROOT),
        }),
    )
    .unwrap();
    assert!(
        read_frame(&mut BufReader::new(client))
            .unwrap()
            .unwrap()
            .get("error")
            .is_some()
    );
    worker.join().unwrap();
    assert_eq!(state.lock().subscribers[&agy.agent_id].len(), 1);
}

#[test]
fn agy_disconnect_before_native_confirmation_is_unknown_and_not_replayed() {
    let (_directory, mut state) = broker();
    let sender = register(&mut state, "one");
    let agy = register_harness(&mut state, "one", "agy");
    state
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    state
        .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
        .unwrap();
    let (message, _) = send(&mut state, &sender, &agy, "no native confirmation");
    let state = Arc::new(Mutex::new(state));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let server_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(
        &mut client,
        &json!({
            "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
            "method":"subscribe","params":agy_receiver_params(AGY_ROOT),
        }),
    )
    .unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap()["event"], "ready");
    assert_eq!(
        read_frame(&mut reader).unwrap().unwrap()["event"],
        "message"
    );
    drop(reader);
    worker.join().unwrap();
    let mut state = state.lock();
    let receipt = &state.store.messages[message["message_id"].as_str().unwrap()];
    assert_eq!(receipt.delivery.outcome.as_deref(), Some("unknown"));
    assert!(receipt.read_at.is_none());
    assert!(receipt.acknowledged_at.is_none());
    assert!(
        send(&mut state, &sender, &agy, "do not replay uncertain pointer")
            .1
            .is_empty()
    );
}

#[cfg(target_os = "linux")]
struct AgyReceiverProcess(std::process::Child);

#[cfg(target_os = "linux")]
impl AgyReceiverProcess {
    fn spawn(stdout: Stdio) -> Self {
        Self(
            Command::new("/bin/sh")
                .args(["-c", "read unused"])
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(stdout)
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn reap(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

#[cfg(target_os = "linux")]
impl Drop for AgyReceiverProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn agy_reaped_receiver_eof_persists_unknown_without_native_callback() {
    let (directory, mut state) = broker();
    let sender = register(&mut state, "one");
    let agy = register_harness(&mut state, "one", "agy");
    state
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // Retain an observer socket until after the child is reaped, forcing EOF
    // cleanup to see a definitely dead PID rather than racing zombie liveness.
    let socket = std::os::fd::OwnedFd::from(client.try_clone().unwrap());
    let mut receiver = AgyReceiverProcess::spawn(Stdio::from(socket));
    let pid = receiver.0.id();
    let mut params = agy_receiver_params(AGY_ROOT);
    params["pid"] = json!(pid);
    state
        .perform(&as_request(&agy, "receiver", params.clone()))
        .unwrap();
    let (message, jobs) = send(
        &mut state,
        &sender,
        &agy,
        "receiver dies before HTTP acknowledgement",
    );
    assert!(jobs.is_empty());
    let state = Arc::new(Mutex::new(state));
    let server_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(
        &mut client,
        &json!({
            "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
            "method":"subscribe","params":params,
        }),
    )
    .unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap()["event"], "ready");
    assert_eq!(
        read_frame(&mut reader).unwrap().unwrap()["event"],
        "message"
    );
    assert_eq!(
        state.lock().store.messages[message["message_id"].as_str().unwrap()]
            .delivery
            .outcome
            .as_deref(),
        Some("pending")
    );
    receiver.reap();
    assert!(process_identity(pid).is_err());
    drop(reader);
    worker.join().unwrap();
    let saved = load_store(directory.path()).unwrap();
    let receipt = &saved.messages[message["message_id"].as_str().unwrap()];
    assert_eq!(receipt.delivery.outcome.as_deref(), Some("unknown"));
    assert_eq!(receipt.wake_failure.as_ref().unwrap().state, "unknown");
    assert!(receipt.read_at.is_none());
    assert!(receipt.acknowledged_at.is_none());
    let agent = &saved.agents[&agy.agent_id];
    assert!(
        alive(agent),
        "the AGY launch remains alive after its helper exits"
    );
    assert!(!receiver_alive(&agent.receiver));
    assert_eq!(agent.notification.as_ref().unwrap().state, "unknown");
}

#[cfg(target_os = "linux")]
#[test]
fn agy_replacement_waits_for_matching_subscription_before_claiming_one_wake() {
    let (_directory, mut state) = broker();
    let sender = register(&mut state, "one");
    let agy = register_harness(&mut state, "one", "agy");
    state
        .perform(&as_request(
            &agy,
            "bind_session",
            json!({"session_id":AGY_ROOT}),
        ))
        .unwrap();
    let mut old_receiver = AgyReceiverProcess::spawn(Stdio::null());
    let mut old_params = agy_receiver_params(AGY_ROOT);
    old_params["pid"] = json!(old_receiver.0.id());
    state
        .perform(&as_request(&agy, "receiver", old_params.clone()))
        .unwrap();
    let old_binding =
        agy_receiver_binding(&state.store.agents[&agy.agent_id], &old_params).unwrap();
    let (stale_sender, stale_receiver) = mpsc::channel();
    state
        .subscribers
        .entry(agy.agent_id.clone())
        .or_default()
        .push(Subscriber {
            id: "old-subscriber-awaiting-eof-cleanup".to_owned(),
            sender: stale_sender,
            binding: Some(old_binding),
        });
    old_receiver.reap();
    let (message, jobs) = send(
        &mut state,
        &sender,
        &agy,
        "wait for replacement subscription",
    );
    assert!(jobs.is_empty());
    assert!(state.store.agents[&agy.agent_id].notification.is_none());
    let generation = state.store.agents[&agy.agent_id].generation;
    let (_, jobs) = state
        .perform(&as_request(&agy, "receiver", agy_receiver_params(AGY_ROOT)))
        .unwrap();
    assert!(
        jobs.is_empty(),
        "stale subscribers cannot claim a replacement receiver's wake"
    );
    let agent = &state.store.agents[&agy.agent_id];
    assert_eq!(agent.generation, generation);
    assert!(agent.notification.is_none());
    assert!(receiver_alive(&agent.receiver));
    assert_eq!(state.subscribers[&agy.agent_id].len(), 1);
    let who = state
        .perform(&as_request(&agy, "whoami", json!({})))
        .unwrap()
        .0;
    assert_eq!(who["readiness"]["notification_connection"], false);
    assert_eq!(who["readiness"]["can_notify"], false);
    assert_eq!(
        state.store.messages[message["message_id"].as_str().unwrap()]
            .delivery
            .state,
        "blocked"
    );
    let state = Arc::new(Mutex::new(state));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let server_state = Arc::clone(&state);
    let worker = std::thread::spawn(move || serve_connection(server, server_state).unwrap());
    write_frame(
        &mut client,
        &json!({
            "protocol":PROTOCOL_VERSION,"agent_id":agy.agent_id,"token":agy.token,
            "method":"subscribe","params":agy_receiver_params(AGY_ROOT),
        }),
    )
    .unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap()["event"], "ready");
    let event = read_frame(&mut reader).unwrap().unwrap();
    assert_eq!(event["message"]["id"], message["message_id"]);
    assert_eq!(event["generation"].as_u64(), Some(generation + 1));
    assert!(stale_receiver.try_recv().is_err());
    {
        let mut state = state.lock();
        let job = WakeJob {
            recipient: agy.agent_id.clone(),
            id: message["message_id"].as_str().unwrap().to_owned(),
            generation: event["generation"].as_u64().unwrap(),
        };
        state
            .perform(&agy_callback(&agy, &job, "accepted", None))
            .unwrap();
        assert!(
            send(
                &mut state,
                &sender,
                &agy,
                "coalesce with the single fresh wake"
            )
            .1
            .is_empty()
        );
        assert_eq!(state.store.agents[&agy.agent_id].generation, generation + 1);
        assert_eq!(
            state.store.agents[&agy.agent_id]
                .notification
                .as_ref()
                .unwrap()
                .state,
            "accepted"
        );
    }
    drop(reader);
    worker.join().unwrap();
}
