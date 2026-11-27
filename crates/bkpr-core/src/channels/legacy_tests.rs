use super::*;
use crate::channels::{
    ChannelAuthority, ChannelRoutes,
    store::{APPLICATION_ID, ATTACHMENT_STAGING_SCHEMA, STORE_NAME, Store},
};
use bkpr_protocol::channels::{ChannelRequest, ChannelResponse};
use serde_json::{Value, json};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};

const NOW: u64 = RETENTION_MS + 50_000;
const A: &str = "12345678-0000-4000-8000-000000000001";
const B: &str = "22345678-0000-4000-8000-000000000002";
const C: &str = "32345678-0000-4000-8000-000000000003";
const PANE_A: &str = "42345678-0000-4000-8000-000000000004";
const PANE_B: &str = "52345678-0000-4000-8000-000000000005";
const PANE_C: &str = "62345678-0000-4000-8000-000000000006";
const WORKSPACE: &str = "72345678-0000-4000-8000-000000000007";

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

fn message_id(sequence: u64) -> String {
    format!("22345678-0000-4000-8000-{sequence:012}")
}

fn enrollment(pane: &str, exited_at: Option<u64>) -> Value {
    json!({
        "pane_id": pane, "harness": "omp", "title": "Last observed title",
        "exited_at": exited_at, "generation": 1, "notification": null,
    })
}

fn receipt(sequence: u64, from: &str, to: &str, body: Option<&str>) -> Value {
    json!({
        "id": message_id(sequence), "from": from, "to": to, "text": body,
        "key": "saved key hash", "digest": "saved body hash", "scope": "workspace",
        "sequence": sequence, "created_at": 100 + sequence, "read_at": null,
        "acknowledged_at": null, "expired_at": null, "notification_id": null,
        "generation": null, "receiver_id": null, "wake_claimed_at": null,
        "wake_state": "blocked", "wake_reason": null, "wake_failure": null,
    })
}

fn ledger(
    messages: impl IntoIterator<Item = Value>,
    exit_a: Option<u64>,
    exit_b: Option<u64>,
) -> Value {
    let messages: serde_json::Map<String, Value> = messages
        .into_iter()
        .map(|message| (message["id"].as_str().unwrap().to_owned(), message))
        .collect();
    let sequence = messages
        .values()
        .map(|m| m["sequence"].as_u64().unwrap())
        .max()
        .unwrap_or(0);
    json!({"version": 1, "agents": {
        (A): enrollment(PANE_A, exit_a), (B): enrollment(PANE_B, exit_b),
        (C): enrollment(PANE_C, Some(NOW - RETENTION_MS)),
    }, "messages": messages, "sequence": sequence})
}

fn save_ledger(root: &Path, ledger: &Value) -> std::path::PathBuf {
    let agents = root.join("agents");
    crate::registry::private_directory(&agents).unwrap();
    let source = agents.join("state.json");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&source)
        .unwrap();
    serde_json::to_writer(file, ledger).unwrap();
    source
}

fn workspace(pane: &str) -> Option<String> {
    matches!(pane, PANE_A | PANE_B).then(|| WORKSPACE.to_owned())
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn authority(actor: &str, kind: ParticipantKind) -> ChannelAuthority {
    ChannelAuthority {
        actor: Participant {
            id: ParticipantId(actor.into()),
            kind,
            display_name: "Viewer".into(),
        },
        launch_id: None,
        writable: true,
        workspace_open: true,
        participants: Vec::new(),
    }
}

fn history(
    store: &mut Store,
    id: &ChannelId,
    actor: &str,
    kind: ParticipantKind,
) -> Result<ChannelResponse> {
    store
        .request(
            ChannelRequest::History {
                channel_id: id.clone(),
                before: None,
                after: None,
                limit: 10,
            },
            |owner| {
                assert_eq!(owner, WORKSPACE);
                Ok(authority(actor, kind))
            },
            ChannelRoutes {
                workspace: Some(WORKSPACE),
                exact_mentions: None,
                publish: &|_| {},
                binding_live: &|_, _| true,
                direct_peer: &|_| Ok(None),
                fence_user_mentions: &|_, _| unreachable!("legacy history read"),
            },
            None,
            |_, _| Ok(false),
        )
        .map(|(response, _)| response)
}

#[test]
fn version_84_restores_exact_history_for_only_the_historical_launch_pair() {
    let root = root();
    let body = "\t exact UTF-8: λ🚀\r\n second line  ";
    let mut acked = receipt(3, A, B, None);
    acked["acknowledged_at"] = json!(NOW - 1);
    let source = save_ledger(
        root.path(),
        &ledger(
            [
                receipt(2, B, A, Some("reverse direction")),
                receipt(1, A, B, Some(body)),
                acked,
                receipt(4, A, C, Some("already expired")),
            ],
            None,
            None,
        ),
    );
    let mut store = Store::open(root.path()).unwrap();
    assert_eq!(
        store.maintain_legacy(root.path(), NOW, &workspace).unwrap(),
        None
    );
    assert!(!source.exists());
    assert_eq!(count(&store, "direct_channels"), 1);
    let channel = ChannelId(
        store
            .connection
            .query_row("SELECT channel_id FROM direct_channels", [], |row| {
                row.get(0)
            })
            .unwrap(),
    );
    let ChannelResponse::History { messages, .. } =
        history(&mut store, &channel, "human", ParticipantKind::User).unwrap()
    else {
        panic!("legacy messages were not visible in history");
    };
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message.id.as_ref(), message_id(1));
    assert_eq!(messages[0].message.sequence, 1);
    assert_eq!(messages[0].message.posted_at_ms, 101);
    assert_eq!(
        messages[0].message.author.id.as_ref(),
        format!("legacy-launch:{A}")
    );
    assert_eq!(messages[0].text, body);
    assert_eq!(messages[1].message.id.as_ref(), message_id(2));
    assert_eq!(messages[1].message.sequence, 2);
    assert_eq!(messages[1].text, "reverse direction");
    assert!(history(&mut store, &channel, A, ParticipantKind::Agent).is_err());
    assert_eq!(count(&store, "notification_work"), 0);
    assert_eq!(count(&store, "channel_batches"), 0);
    assert_eq!(count(&store, "positions"), 0);
}

#[test]
fn unresolved_bodies_are_never_reclassified_and_expire_individually_across_reopen() {
    let root = root();
    let exact_early = "first unresolved\r\n";
    let exact_late = "late unresolved λ";
    let source = save_ledger(
        root.path(),
        &ledger(
            [
                receipt(1, A, B, Some(exact_early)),
                receipt(2, B, A, Some(exact_late)),
            ],
            None,
            Some(NOW - RETENTION_MS + 5),
        ),
    );
    {
        let mut store = Store::open(root.path()).unwrap();
        assert_eq!(
            store.maintain_legacy(root.path(), NOW, &|_| None).unwrap(),
            Some(NOW + 5)
        );
        assert_eq!(count(&store, "posts"), 0);
    }
    let read = || -> Quarantine { serde_json::from_reader(File::open(&source).unwrap()).unwrap() };
    let held = read();
    assert_eq!(held.retained[0].text, exact_early);
    assert_eq!(held.retained[1].text, exact_late);
    let mut store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .maintain_legacy(root.path(), NOW + 5, &workspace)
            .unwrap(),
        Some(NOW + RETENTION_MS)
    );
    assert_eq!(count(&store, "posts"), 0);
    let held = read();
    assert_eq!(held.retained.len(), 1);
    assert_eq!(held.retained[0].text, exact_late);
    assert_eq!(
        store
            .maintain_legacy(root.path(), NOW + RETENTION_MS, &workspace)
            .unwrap(),
        None
    );
    assert!(!source.exists());
    assert!(!source.with_file_name(STAGED.to_str().unwrap()).exists());
}

#[test]
fn only_workspace_scope_allows_proof_through_the_recipient_pane() {
    let root = root();
    let mut broader = receipt(2, B, A, Some("recipient is not the owner"));
    broader["scope"] = json!("session");
    let source = save_ledger(
        root.path(),
        &ledger(
            [
                receipt(1, B, A, Some("recipient proves the workspace")),
                broader,
                receipt(3, C, A, Some("conflicting workspace evidence")),
            ],
            None,
            None,
        ),
    );
    let mut store = Store::open(root.path()).unwrap();
    let owns = |pane: &str| match pane {
        PANE_A => Some(WORKSPACE.to_owned()),
        PANE_C => Some("another-workspace".to_owned()),
        _ => None,
    };
    assert_eq!(
        store.maintain_legacy(root.path(), NOW, &owns).unwrap(),
        Some(NOW + RETENTION_MS)
    );
    assert_eq!(count(&store, "posts"), 1);
    let body: String = store
        .connection
        .query_row("SELECT body FROM posts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(body, "recipient proves the workspace");
    let held: Quarantine = serde_json::from_reader(File::open(&source).unwrap()).unwrap();
    assert_eq!(held.retained.len(), 2);
    assert_eq!(held.retained[0].text, "recipient is not the owner");
    assert_eq!(held.retained[1].text, "conflicting workspace evidence");
}

#[test]
fn interrupted_publication_reuses_the_committed_import_and_original_expiry() {
    let root = root();
    let mut unprovable = receipt(2, C, A, Some("unprovable before crash"));
    unprovable["scope"] = json!("session");
    let source_path = save_ledger(
        root.path(),
        &ledger(
            [receipt(1, A, B, Some("imported before crash")), unprovable],
            None,
            None,
        ),
    );
    let directory = File::open(source_path.parent().unwrap()).unwrap();
    let source = existing(&directory, SOURCE).unwrap().unwrap();
    {
        let mut store = Store::open(root.path()).unwrap();
        stage_import(
            &mut store.connection,
            &directory,
            &stamp(&source).unwrap(),
            decode::<Document>(&source).unwrap().into_ledger().unwrap(),
            NOW,
            &workspace,
        )
        .unwrap();
        assert_eq!(count(&store, "posts"), 1);
        assert_eq!(decode::<Document>(&source).unwrap().version, 1);
    }

    let mut store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .maintain_legacy(root.path(), NOW + 500, &|_| panic!(
                "committed import was replayed"
            ))
            .unwrap(),
        Some(NOW + RETENTION_MS),
    );
    assert_eq!(count(&store, "posts"), 1);
    let held: Quarantine = serde_json::from_reader(File::open(&source_path).unwrap()).unwrap();
    assert_eq!(held.retained.len(), 1);
    assert_eq!(held.retained[0].text, "unprovable before crash");
}

fn v9(root: &Path, snapshot: &Value) -> ChannelId {
    // a1a774c wrote ordinary channel posts and a body-less singleton snapshot
    // together, with the pre-ordered wake columns and indexes below.
    let file = root.join(STORE_NAME);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .unwrap();
    let mut db = Connection::open(file).unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    db.execute_batch(include_str!("wakes.sql")).unwrap();
    db.execute_batch(ATTACHMENT_STAGING_SCHEMA).unwrap();
    db.execute_batch(include_str!("direct.sql")).unwrap();
    db.execute_batch(
        "CREATE UNIQUE INDEX active_batch_launch ON channel_batches(launch_id) WHERE outcome IS NULL;
         CREATE INDEX pending_launch_wakes ON notification_work(launch_id,binding_revision,channel_id)
             WHERE pending=1 AND launch_id IS NOT NULL;
         CREATE TABLE agent_state (singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot BLOB NOT NULL) STRICT;",
    ).unwrap();
    db.pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    db.pragma_update(None, "user_version", 9).unwrap();
    let tx = db.transaction().unwrap();
    let channel = ChannelId(Uuid::new_v4().to_string());
    let a = authority(A, ParticipantKind::Agent).actor;
    let b = authority(B, ParticipantKind::Agent).actor;
    insert_direct_channel(&tx, &channel, WORKSPACE, &a, &b).unwrap();
    let message = MessageSummary {
        id: MessageId(message_id(1)),
        sequence: 1,
        author: a,
        posted_at_ms: 101,
        intent: PostIntent::Note,
        mentions: Vec::new(),
        mention_spans: Vec::new(),
        reply_to: None,
        attachments: Vec::new(),
        source: None,
        role: None,
    };
    insert_post(
        &tx,
        &channel,
        &message,
        &format!("\u{1}{}", message.id.as_ref()),
        &[],
        "canonical SQLite body \r\nλ",
        (1, false),
    )
    .unwrap();
    tx.execute(
        "INSERT INTO agent_state VALUES (1,?1)",
        [serde_json::to_vec(snapshot).unwrap()],
    )
    .unwrap();
    tx.commit().unwrap();
    channel
}

#[test]
fn canonical_v9_post_wins_and_v9_purge_cannot_be_undone_by_old_json() {
    let root = root();
    let old = ledger(
        [
            receipt(1, A, B, Some("stale file body")),
            receipt(2, A, B, Some("subsequently purged")),
            receipt(3, A, B, Some("provable remaining body")),
        ],
        None,
        Some(NOW - 1),
    );
    let mut newer = old.clone();
    for message in newer["messages"].as_object_mut().unwrap().values_mut() {
        message.as_object_mut().unwrap().remove("text");
    }
    newer["messages"][message_id(1)]["acknowledged_at"] = json!(NOW - 1);
    newer["messages"][message_id(2)]["expired_at"] = json!(NOW - 1);
    let channel = v9(root.path(), &newer);
    let source = save_ledger(root.path(), &old);
    let mut store = Store::open(root.path()).unwrap();
    assert_eq!(
        store.maintain_legacy(root.path(), NOW, &workspace).unwrap(),
        None
    );
    assert!(!source.exists());
    assert_eq!(count(&store, "posts"), 2);
    let ChannelResponse::History { messages, .. } =
        history(&mut store, &channel, "human", ParticipantKind::User).unwrap()
    else {
        panic!("v9 post was not visible in history");
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "canonical SQLite body \r\nλ");
    assert_eq!(messages[0].message.author.id.as_ref(), A);
    let (body, workspace): (String, String) = store.connection.query_row(
        "SELECT p.body,c.workspace_id FROM posts p JOIN channels c ON c.id=p.channel_id WHERE p.id=?1",
        [message_id(3)], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(body, "provable remaining body");
    assert_eq!(workspace, WORKSPACE);
    assert_eq!(count(&store, "notification_work"), 0);
    assert!(
        store
            .connection
            .prepare("SELECT snapshot FROM agent_state")
            .is_err()
    );
}

#[test]
fn symlinked_or_hard_linked_ledger_is_left_untouched() {
    let root = root();
    let source = save_ledger(
        root.path(),
        &ledger([receipt(1, A, B, Some("private"))], None, None),
    );
    let other = root.path().join("other.json");
    fs::hard_link(&source, &other).unwrap();
    let mut store = Store::open(root.path()).unwrap();
    assert!(store.maintain_legacy(root.path(), NOW, &workspace).is_err());
    let original = fs::read(&other).unwrap();
    assert_eq!(fs::read(&source).unwrap(), original);
    fs::remove_file(&source).unwrap();
    symlink(&other, &source).unwrap();
    assert!(store.maintain_legacy(root.path(), NOW, &workspace).is_err());
    assert!(fs::symlink_metadata(&source).unwrap().is_symlink());
    assert_eq!(fs::read(&other).unwrap(), original);
    assert_eq!(count(&store, "posts"), 0);
}
