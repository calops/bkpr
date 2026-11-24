use super::{
    push_transport::{Publisher, PushSendOutcome, validate_subscription},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{
    Notification, NotificationKind, NotificationScope, PushDestinationInfo, PushDestinationPage,
    PushDiagnostics, PushEndpointPolicy, PushEnrollmentReceipt, PushJobDiagnostic,
    PushMutationReceipt, PushPublicKey, PushRequest, PushRevision, PushSubscription,
    PushTestReceipt,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const CHALLENGE_MS: u64 = 10 * 60 * 1000;
const DELIVERY_MS: u64 = 5 * 60 * 1000;
const MAX_DESTINATIONS: i64 = 32;
const MAX_REQUESTS: i64 = 4096;
const RECEIPT_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_QUEUED: i64 = 512;

#[derive(Serialize, Deserialize)]
struct Destination {
    #[serde(default)]
    subscription: Option<PushSubscription>,
    policy: PushEndpointPolicy,
    scope: NotificationScope,
    kinds: Vec<NotificationKind>,
    replaces: Option<PushRevision>,
    #[serde(default)]
    device_label: Option<String>,
}
/// One durably claimed network operation; never formatted with secrets.
pub(super) struct Job {
    pub id: String,
    pub destination_id: String,
    pub generation: u64,
    pub subscription: PushSubscription,
    pub policy: PushEndpointPolicy,
    pub payload: Vec<u8>,
    pub ttl: u32,
    pub private_pem: Arc<str>,
}
pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS push_key (singleton INTEGER PRIMARY KEY CHECK(singleton=1), private_pem TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS push_destinations (id TEXT NOT NULL, generation INTEGER NOT NULL, state TEXT NOT NULL, expires INTEGER NOT NULL, nonce TEXT, descriptor TEXT NOT NULL, lost INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(id,generation));
        CREATE TABLE IF NOT EXISTS push_requests (id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, response TEXT NOT NULL, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS push_identities (id TEXT PRIMARY KEY, generation INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS push_health (singleton INTEGER PRIMARY KEY CHECK(singleton=1), dropped_jobs INTEGER NOT NULL);
        INSERT OR IGNORE INTO push_health VALUES(1,0);
        CREATE TABLE IF NOT EXISTS push_jobs (id TEXT PRIMARY KEY, destination TEXT NOT NULL, generation INTEGER NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL, due INTEGER NOT NULL, expires INTEGER NOT NULL, attempts INTEGER NOT NULL, payload BLOB NOT NULL, result TEXT);
        CREATE INDEX IF NOT EXISTS push_due ON push_jobs(state,due);
        UPDATE push_jobs SET state='unknown',result='host_restart_after_dispatch',payload=x'' WHERE state='dispatching';")?;
    Ok(())
}
fn key(connection: &Connection) -> Result<String> {
    if let Some(key) = connection
        .query_row(
            "SELECT private_pem FROM push_key WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(key);
    }
    ensure!(
        !connection.query_row("SELECT EXISTS(SELECT 1 FROM push_destinations)", [], |r| {
            r.get::<_, bool>(0)
        })?,
        "push application-server key unavailable"
    );
    let key = Publisher::generate_key()?;
    connection.execute("INSERT INTO push_key VALUES(1,?1)", [&key])?;
    Ok(key)
}
fn revision(
    connection: &Connection,
    id: &str,
    generation: u64,
    state: Option<&str>,
) -> Result<String> {
    let row: Option<(u64, String)> = connection
        .query_row(
            "SELECT generation,state FROM push_destinations WHERE id=?1 AND generation=?2",
            params![id, generation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (actual, actual_state) = row.context("push destination not found")?;
    ensure!(generation == actual, "push destination generation changed");
    if let Some(state) = state {
        ensure!(
            actual_state == state,
            "push destination is not in the required state"
        );
    }
    Ok(actual_state)
}
fn replaceable(connection: &Connection, id: &str, generation: u64) -> Result<()> {
    let state = revision(connection, id, generation, None)?;
    ensure!(
        state == "active" || state == "expired",
        "push replacement requires an active or expired destination"
    );
    ensure!(!connection.query_row("SELECT EXISTS(SELECT 1 FROM push_destinations WHERE id=?1 AND state='active' AND generation!=?2)",params![id,generation],|r|r.get::<_,bool>(0))?,"push destination generation changed");
    Ok(())
}
fn fingerprint(request: &PushRequest) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(request)?)
    ))
}
fn remembered(connection: &Connection, id: &str, fingerprint: &str) -> Result<Option<Value>> {
    if let Some((body, response)) = connection
        .query_row(
            "SELECT fingerprint,response FROM push_requests WHERE id=?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        ensure!(
            body == fingerprint,
            "push request_id reused with different arguments"
        );
        return Ok(Some(serde_json::from_str(&response)?));
    }
    ensure!(
        connection.query_row("SELECT COUNT(*) FROM push_requests", [], |r| r
            .get::<_, i64>(0))?
            < MAX_REQUESTS,
        "push idempotency ledger capacity reached"
    );
    let bytes:u64=connection.query_row("SELECT COALESCE(SUM(length(CAST(fingerprint AS BLOB))+length(CAST(response AS BLOB))),0) FROM push_requests",[],|r|r.get(0))?;
    ensure!(
        bytes
            .saturating_add(fingerprint.len() as u64)
            .saturating_add(1024)
            <= 4 * 1024 * 1024,
        "push idempotency ledger byte capacity reached"
    );
    Ok(None)
}
fn remember(
    tx: &Transaction<'_>,
    id: &str,
    fingerprint: &str,
    response: &Value,
    expires: u64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO push_requests VALUES(?1,?2,?3,?4)",
        params![id, fingerprint, serde_json::to_string(response)?, expires],
    )?;
    Ok(())
}
fn mutation(
    id: &str,
    generation: u64,
    state: &str,
    revoked: bool,
    expires: Option<u64>,
) -> Result<Value> {
    Ok(serde_json::to_value(PushMutationReceipt {
        destination_id: id.into(),
        generation,
        state: state.into(),
        in_flight_outcome: revoked.then(|| "unknown_until_completed".into()),
        idempotency_expires_at_ms: expires,
    })?)
}
fn enqueue(
    tx: &Transaction<'_>,
    destination: &str,
    generation: u64,
    kind: &str,
    payload: &[u8],
    now: u64,
    expires: u64,
) -> Result<Option<String>> {
    tx.execute("DELETE FROM push_jobs WHERE id IN (SELECT id FROM push_jobs WHERE state NOT IN ('queued','dispatching') ORDER BY rowid DESC LIMIT -1 OFFSET 128)",[])?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM push_jobs WHERE state IN ('queued','dispatching')",
        [],
        |r| r.get(0),
    )?;
    let local: i64 = tx.query_row(
        "SELECT COUNT(*) FROM push_jobs WHERE destination=?1 AND state IN ('queued','dispatching')",
        [destination],
        |r| r.get(0),
    )?;
    if count >= MAX_QUEUED || local >= 64 {
        tx.execute("UPDATE push_health SET dropped_jobs=CASE WHEN dropped_jobs<9223372036854775807 THEN dropped_jobs+1 ELSE dropped_jobs END WHERE singleton=1",[])?;
        tx.execute("UPDATE push_destinations SET lost=CASE WHEN lost<9223372036854775807 THEN lost+1 ELSE lost END WHERE id=?1 AND generation=?2",params![destination,generation])?;
        return Ok(None);
    }
    ensure!(
        payload.len() <= 3000,
        "push payload exceeds encrypted transport bound"
    );
    let id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO push_jobs VALUES(?1,?2,?3,?4,'queued',?5,?6,0,?7,NULL)",
        params![id, destination, generation, kind, now, expires, payload],
    )?;
    Ok(Some(id))
}
/// Insert jobs inside the notification's own acceptance transaction.
pub(super) fn enqueue_notification(
    tx: &Transaction<'_>,
    notification: &Notification,
    store_id: &str,
    now: u64,
) -> Result<bool> {
    let session = notification.source.session();
    let payload = serde_json::to_vec(
        &json!({"type":"notification","notification_id":notification.id,"store_id":store_id,"session_id":session.session_id,"session_name":session.name,"kind":notification.kind,"attention_kind":notification.attention_kind}),
    )?;
    let mut statement = tx.prepare(
        "SELECT id,generation,descriptor FROM push_destinations WHERE state='active' ORDER BY id",
    )?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut created = false;
    for row in rows {
        let (id, generation, descriptor) = row?;
        let destination: Destination = serde_json::from_str(&descriptor)?;
        if super::storage::matches_scope(&destination.scope, &notification.source)
            && destination.kinds.contains(&notification.kind)
        {
            created |= enqueue(
                tx,
                &id,
                generation,
                "notification",
                &payload,
                now,
                now.saturating_add(DELIVERY_MS),
            )?
            .is_some();
        }
    }
    Ok(created)
}
impl Store {
    fn master_key(&mut self) -> Result<&str> {
        if self.push_key.is_none() {
            self.push_key = Some(Arc::from(key(&self.connection)?));
        }
        Ok(self.push_key.as_deref().unwrap())
    }
    pub(super) fn push_request(&mut self, request: PushRequest, now: u64) -> Result<Value> {
        let dirty = self.push_dirty;
        let cached = self.push_deadline_cache;
        let result = self.push_request_inner(request, now);
        if let Err(error) = &result {
            self.push_dirty = dirty;
            self.push_deadline_cache = cached;
            if let Some(sql) = error.downcast_ref::<rusqlite::Error>() {
                if matches!(sql,rusqlite::Error::SqliteFailure(code,_) if matches!(code.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked|rusqlite::ErrorCode::ConstraintViolation))
                {
                    return Err(anyhow::anyhow!(
                        "push storage temporarily busy or conflicted; no successful receipt; retry identical idempotency key or inspect state"
                    ));
                }
                self.mark_failed();
                return Err(anyhow::anyhow!(
                    "push storage failed; durable outcome unknown"
                ));
            }
        }
        result
    }
    fn push_request_inner(&mut self, request: PushRequest, now: u64) -> Result<Value> {
        request.validate()?;
        self.push_dirty = true;
        self.push_deadline_cache = None;
        self.push_expire(now)?;
        match &request {
            PushRequest::PublicKey {} => Ok(serde_json::to_value(PushPublicKey {
                public_key: Publisher::new(self.master_key()?)?.public_key(),
            })?),
            PushRequest::List {} => {
                let mut statement=self.connection.prepare("SELECT id,generation,state,expires,lost,descriptor FROM push_destinations ORDER BY CASE WHEN state IN ('active','pending') THEN 0 ELSE 1 END,rowid DESC LIMIT 128")?;
                let rows = statement.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, u64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })?;
                let mut destinations = Vec::new();
                for row in rows {
                    let (id, generation, state, expires, lost, descriptor) = row?;
                    let descriptor: Option<Destination> = if descriptor.is_empty() {
                        None
                    } else {
                        Some(serde_json::from_str(&descriptor)?)
                    };
                    let (device_label, scope, kinds) = descriptor.map_or((None, None, None), |d| {
                        (d.device_label, Some(d.scope), Some(d.kinds))
                    });
                    let enabled = kinds.as_ref().map(|kinds| !kinds.is_empty());
                    destinations.push(PushDestinationInfo {
                        destination_id: id,
                        generation,
                        state,
                        challenge_expires_at_ms: expires,
                        dropped_jobs: lost,
                        device_label,
                        scope,
                        kinds,
                        enabled,
                    });
                }
                Ok(serde_json::to_value(PushDestinationPage {
                    destinations,
                    retained_limit: 128,
                })?)
            }
            PushRequest::Update {
                request_id,
                destination_id,
                generation,
                scope,
                kinds,
            } => {
                let fingerprint = fingerprint(&request)?;
                if let Some(response) = remembered(&self.connection, request_id, &fingerprint)? {
                    return Ok(response);
                }
                revision(
                    &self.connection,
                    destination_id,
                    *generation,
                    Some("active"),
                )?;
                let (descriptor, nonce): (String, String) = self.connection.query_row(
                    "SELECT descriptor,nonce FROM push_destinations WHERE id=?1 AND generation=?2",
                    params![destination_id, generation],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let mut descriptor: Destination = serde_json::from_str(&descriptor)?;
                descriptor.scope = scope.clone();
                descriptor.kinds = kinds.clone();
                descriptor.replaces = None;
                let next = self
                    .connection
                    .query_row(
                        "SELECT generation FROM push_identities WHERE id=?1",
                        [destination_id],
                        |r| r.get::<_, u64>(0),
                    )?
                    .checked_add(1)
                    .context("push generation exhausted")?;
                ensure!(next <= i64::MAX as u64, "push generation exhausted");
                let tx = self.connection.transaction()?;
                tx.execute(
                    "UPDATE push_identities SET generation=?2 WHERE id=?1",
                    params![destination_id, next],
                )?;
                tx.execute("UPDATE push_destinations SET state='revoked',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE id=?1", [destination_id])?;
                tx.execute("UPDATE push_jobs SET state='revoked',result='destination_filters_updated',payload=x'' WHERE destination=?1 AND state='queued'",[destination_id])?;
                tx.execute(
                    "INSERT INTO push_destinations VALUES(?1,?2,'active',0,?3,?4,0)",
                    params![
                        destination_id,
                        next,
                        nonce,
                        serde_json::to_string(&descriptor)?
                    ],
                )?;
                let response = mutation(
                    destination_id,
                    next,
                    "active",
                    false,
                    Some(now.saturating_add(RECEIPT_MS)),
                )?;
                remember(
                    &tx,
                    request_id,
                    &fingerprint,
                    &response,
                    now.saturating_add(RECEIPT_MS),
                )?;
                tx.commit()?;
                Ok(response)
            }
            PushRequest::Diagnostics {} => {
                let mut statement=self.connection.prepare("SELECT id,destination,generation,kind,state,attempts,result FROM push_jobs ORDER BY rowid DESC LIMIT 128")?;
                let rows = statement.query_map([], |r| {
                    Ok(PushJobDiagnostic {
                        job_id: r.get(0)?,
                        destination_id: r.get(1)?,
                        generation: r.get(2)?,
                        kind: r.get(3)?,
                        state: r.get(4)?,
                        attempts: r.get(5)?,
                        result: r.get(6)?,
                    })
                })?;
                let dropped: u64 = self.connection.query_row(
                    "SELECT dropped_jobs FROM push_health WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )?;
                Ok(serde_json::to_value(PushDiagnostics {
                    jobs: rows.collect::<rusqlite::Result<Vec<_>>>()?,
                    dropped_jobs: dropped,
                    retained_limit: 128,
                    acceptance_is_delivery: false,
                })?)
            }
            PushRequest::Enroll {
                request_id,
                subscription,
                policy,
                scope,
                kinds,
                replaces,
                device_label,
            } => {
                let fingerprint = fingerprint(&request)?;
                if let Some(response) = remembered(&self.connection, request_id, &fingerprint)? {
                    return Ok(response);
                }
                validate_subscription(subscription, policy)?;
                let consumes_slot = if let Some(old) = replaces {
                    !self.connection.query_row("SELECT EXISTS(SELECT 1 FROM push_destinations WHERE id=?1 AND state IN ('active','pending'))",[&old.destination_id],|r|r.get::<_,bool>(0))?
                } else {
                    true
                };
                if consumes_slot {
                    ensure!(self.connection.query_row("SELECT COUNT(DISTINCT id) FROM push_destinations WHERE state IN ('active','pending')",[],|r|r.get::<_,i64>(0))?<MAX_DESTINATIONS,"push destination capacity reached");
                }
                ensure!(
                    self.connection.query_row(
                        "SELECT COUNT(*) FROM push_destinations WHERE state='pending'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )? < MAX_DESTINATIONS,
                    "push pending enrollment capacity reached"
                );
                let retained_bytes:u64=self.connection.query_row("SELECT COALESCE(SUM(length(CAST(descriptor AS BLOB))),0) FROM push_destinations",[],|r|r.get(0))?;
                ensure!(
                    retained_bytes.saturating_add(16 * 1024) <= 4 * 1024 * 1024,
                    "push subscription byte capacity reached"
                );
                if let Some(old) = replaces {
                    replaceable(&self.connection, &old.destination_id, old.generation)?;
                }
                self.master_key()?;
                let id = replaces.as_ref().map_or_else(
                    || Uuid::new_v4().to_string(),
                    |old| old.destination_id.clone(),
                );
                let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
                let generation = if replaces.is_some() {
                    self.connection
                        .query_row(
                            "SELECT generation FROM push_identities WHERE id=?1",
                            [&id],
                            |r| r.get::<_, u64>(0),
                        )?
                        .saturating_add(1)
                } else {
                    1
                };
                ensure!(
                    generation <= i64::MAX as u64,
                    "push destination generation exhausted"
                );
                let expires = now.saturating_add(CHALLENGE_MS);
                let payload = serde_json::to_vec(
                    &json!({"type":"enrollment_challenge","destination_id":id,"generation":generation,"nonce":nonce,"expires_at_ms":expires}),
                )?;
                let device_label = if device_label.is_some() {
                    device_label.clone()
                } else if let Some(old) = replaces {
                    let old: String = self.connection.query_row(
                        "SELECT descriptor FROM push_destinations WHERE id=?1 AND generation=?2",
                        params![old.destination_id, old.generation],
                        |r| r.get(0),
                    )?;
                    serde_json::from_str::<Destination>(&old)?.device_label
                } else {
                    None
                };
                let descriptor = Destination {
                    subscription: Some(subscription.clone()),
                    policy: policy.clone(),
                    scope: scope.clone(),
                    kinds: kinds.clone(),
                    replaces: replaces.clone(),
                    device_label,
                };
                let tx = self.connection.transaction()?;
                tx.execute("INSERT INTO push_identities VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation",params![id,generation])?;
                let nonce_hash = format!("{:x}", Sha256::digest(nonce.as_bytes()));
                tx.execute(
                    "INSERT INTO push_destinations VALUES(?1,?2,'pending',?3,?4,?5,0)",
                    params![
                        id,
                        generation,
                        expires,
                        nonce_hash,
                        serde_json::to_string(&descriptor)?
                    ],
                )?;
                let job = enqueue(&tx, &id, generation, "challenge", &payload, now, expires)?
                    .context("push queue saturated; enrollment not accepted")?;
                let response = serde_json::to_value(PushEnrollmentReceipt {
                    destination_id: id,
                    generation,
                    state: "pending".into(),
                    challenge_expires_at_ms: expires,
                    job_id: job,
                    idempotency_expires_at_ms: now.saturating_add(RECEIPT_MS),
                })?;
                remember(
                    &tx,
                    request_id,
                    &fingerprint,
                    &response,
                    now.saturating_add(RECEIPT_MS),
                )?;
                tx.commit()?;
                Ok(response)
            }
            PushRequest::Confirm {
                destination_id,
                generation,
                nonce,
            } => {
                let state = revision(&self.connection, destination_id, *generation, None)?;
                ensure!(
                    state == "pending" || state == "active",
                    "push challenge expired or destination revoked"
                );
                let (expected, descriptor): (String, String) = self.connection.query_row(
                    "SELECT nonce,descriptor FROM push_destinations WHERE id=?1 AND generation=?2",
                    params![destination_id, generation],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let proof = format!("{:x}", Sha256::digest(nonce.as_bytes()));
                let mut mismatch = expected.len() ^ proof.len();
                for (a, b) in expected.bytes().zip(proof.bytes()) {
                    mismatch |= usize::from(a ^ b);
                }
                ensure!(mismatch == 0, "push challenge proof rejected");
                if state == "active" {
                    return mutation(destination_id, *generation, "active", false, None);
                }
                let descriptor: Destination = serde_json::from_str(&descriptor)?;
                let tx = self.connection.transaction()?;
                if let Some(old) = descriptor.replaces {
                    replaceable(&tx, &old.destination_id, old.generation)?;
                    tx.execute("UPDATE push_destinations SET state='revoked',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE id=?1 AND generation=?2",params![old.destination_id,old.generation])?;
                    tx.execute("UPDATE push_jobs SET state='revoked',result='destination_replaced',payload=x'' WHERE destination=?1 AND generation=?2 AND state='queued'",params![old.destination_id,old.generation])?;
                }
                tx.execute("UPDATE push_destinations SET state='revoked',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE id=?1 AND generation!=?2 AND state='pending'",params![destination_id,generation])?;
                tx.execute("UPDATE push_jobs SET state='revoked',result='superseded_enrollment',payload=x'' WHERE destination=?1 AND generation!=?2 AND state='queued'",params![destination_id,generation])?;
                tx.execute(
                    "UPDATE push_destinations SET state='active' WHERE id=?1 AND generation=?2",
                    params![destination_id, generation],
                )?;
                tx.execute("UPDATE push_jobs SET state='confirmed',result='challenge_confirmed',payload=x'' WHERE destination=?1 AND generation=?2 AND state='queued' AND kind='challenge'",params![destination_id,generation])?;
                tx.commit()?;
                mutation(destination_id, *generation, "active", false, None)
            }
            PushRequest::Revoke {
                destination_id,
                generation,
            } => {
                let state = revision(&self.connection, destination_id, *generation, None)?;
                if state == "revoked" {
                    return mutation(destination_id, *generation, "revoked", true, None);
                }
                ensure!(state=="revoked" || !self.connection.query_row("SELECT EXISTS(SELECT 1 FROM push_destinations WHERE id=?1 AND generation>?2 AND state='active')",params![destination_id,generation],|r|r.get::<_,bool>(0))?,"push destination generation changed");
                let tx = self.connection.transaction()?;
                tx.execute("UPDATE push_destinations SET state='revoked',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE id=?1 AND generation>=?2",params![destination_id,generation])?;
                tx.execute("UPDATE push_jobs SET state='revoked',result='destination_revoked',payload=x'' WHERE destination=?1 AND generation>=?2 AND state='queued'",params![destination_id,generation])?;
                tx.commit()?;
                mutation(destination_id, *generation, "revoked", true, None)
            }
            PushRequest::Test {
                request_id,
                destination_id,
                generation,
            } => {
                let fingerprint = fingerprint(&request)?;
                if let Some(response) = remembered(&self.connection, request_id, &fingerprint)? {
                    return Ok(response);
                }
                revision(
                    &self.connection,
                    destination_id,
                    *generation,
                    Some("active"),
                )?;
                let payload = serde_json::to_vec(&json!({"type":"test","request_id":request_id}))?;
                let tx = self.connection.transaction()?;
                let job = enqueue(
                    &tx,
                    destination_id,
                    *generation,
                    "test",
                    &payload,
                    now,
                    now.saturating_add(DELIVERY_MS),
                )?
                .context("push queue saturated; test not accepted")?;
                let response = serde_json::to_value(PushTestReceipt {
                    job_id: job,
                    state: "queued".into(),
                    idempotency_expires_at_ms: now.saturating_add(RECEIPT_MS),
                })?;
                remember(
                    &tx,
                    request_id,
                    &fingerprint,
                    &response,
                    now.saturating_add(RECEIPT_MS),
                )?;
                tx.commit()?;
                Ok(response)
            }
        }
    }
    pub(super) fn push_expire(&mut self, now: u64) -> Result<()> {
        self.push_dirty = true;
        self.push_deadline_cache = None;
        let tx = self.connection.transaction()?;
        tx.execute("UPDATE push_destinations SET state='expired',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE state='pending' AND expires<=?1",[now])?;
        tx.execute("UPDATE push_jobs SET state='expired',result='ttl_expired',payload=x'' WHERE state='queued' AND expires<=?1",[now])?;
        tx.execute("UPDATE push_jobs SET state='revoked',result='destination_inactive',payload=x'' WHERE state='queued' AND NOT EXISTS(SELECT 1 FROM push_destinations d WHERE d.id=push_jobs.destination AND d.generation=push_jobs.generation AND ((d.state='pending' AND push_jobs.kind='challenge') OR (d.state='active' AND push_jobs.kind!='challenge')))",[])?;
        tx.execute("DELETE FROM push_requests WHERE expires<=?1", [now])?;
        tx.execute("DELETE FROM push_destinations WHERE rowid IN (SELECT d.rowid FROM push_destinations d WHERE d.state IN ('revoked','expired') AND NOT EXISTS(SELECT 1 FROM push_jobs j WHERE j.destination=d.id AND j.generation=d.generation AND j.state='dispatching') ORDER BY d.rowid DESC LIMIT -1 OFFSET 128)",[])?;
        tx.execute("DELETE FROM push_identities WHERE NOT EXISTS(SELECT 1 FROM push_destinations d WHERE d.id=push_identities.id) AND NOT EXISTS(SELECT 1 FROM push_jobs j WHERE j.destination=push_identities.id AND j.state='dispatching')",[])?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn push_deadline(&mut self, dispatch_available: bool) -> Result<Option<u64>> {
        if let Some((available, deadline)) = self.push_deadline_cache
            && available == dispatch_available
        {
            return Ok(deadline);
        }
        let sql = if dispatch_available {
            "SELECT MIN(deadline) FROM (SELECT MIN(due) AS deadline FROM push_jobs j WHERE state='queued' AND NOT EXISTS(SELECT 1 FROM push_jobs a WHERE a.destination=j.destination AND a.state='dispatching') UNION ALL SELECT MIN(expires) FROM push_destinations WHERE state='pending' UNION ALL SELECT MIN(expires) FROM push_requests)"
        } else {
            "SELECT MIN(deadline) FROM (SELECT MIN(expires) AS deadline FROM push_destinations WHERE state='pending' UNION ALL SELECT MIN(expires) FROM push_requests)"
        };
        let deadline = self.connection.query_row(sql, [], |r| r.get(0))?;
        self.push_deadline_cache = Some((dispatch_available, deadline));
        Ok(deadline)
    }
    pub(super) fn push_claim(&mut self, now: u64) -> Result<Option<Job>> {
        self.push_expire(now)?;
        let tx = self.connection.transaction()?;
        let row=tx.query_row("SELECT j.id,j.destination,j.generation,j.payload,j.expires,d.descriptor FROM push_jobs j JOIN push_destinations d ON d.id=j.destination AND d.generation=j.generation WHERE j.state='queued' AND j.due<=?1 AND ((d.state='active' AND j.kind!='challenge') OR(d.state='pending' AND j.kind='challenge')) AND NOT EXISTS(SELECT 1 FROM push_jobs a WHERE a.destination=j.destination AND a.state='dispatching') ORDER BY j.due,j.rowid LIMIT 1",[now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,Vec<u8>>(3)?,r.get::<_,u64>(4)?,r.get::<_,String>(5)?))).optional()?;
        let Some((id, destination_id, generation, payload, expires, descriptor)) = row else {
            return Ok(None);
        };
        let destination: Destination = serde_json::from_str(&descriptor)?;
        let private_pem = if let Some(key) = &self.push_key {
            key.clone()
        } else {
            let key: String = tx.query_row(
                "SELECT private_pem FROM push_key WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            Arc::<str>::from(key)
        };
        tx.execute(
            "UPDATE push_jobs SET state='dispatching',attempts=attempts+1 WHERE id=?1",
            [&id],
        )?;
        tx.commit()?;
        if self.push_key.is_none() {
            self.push_key = Some(private_pem.clone());
        }
        Ok(Some(Job {
            id,
            destination_id,
            generation,
            subscription: destination
                .subscription
                .context("push subscription was retired")?,
            policy: destination.policy,
            payload,
            ttl: (expires.saturating_sub(now) / 1000).clamp(1, 86400) as u32,
            private_pem,
        }))
    }
    pub(super) fn push_complete(
        &mut self,
        id: &str,
        outcome: PushSendOutcome,
        now: u64,
    ) -> Result<()> {
        self.push_dirty = true;
        self.push_deadline_cache = None;
        let tx = self.connection.transaction()?;
        let (destination,generation,attempts,expires):(String,u64,u32,u64)=tx.query_row("SELECT destination,generation,attempts,expires FROM push_jobs WHERE id=?1 AND state='dispatching'",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let (state, result, retry) = match outcome {
            PushSendOutcome::Accepted => (
                "accepted",
                "provider_accepted_not_device_delivery".to_owned(),
                None,
            ),
            PushSendOutcome::Expired => {
                tx.execute("UPDATE push_destinations SET state='expired',nonce=NULL,descriptor=json_remove(descriptor,'$.subscription') WHERE id=?1 AND generation=?2 AND state IN ('active','pending')",params![destination,generation])?;
                ("expired", "provider_subscription_expired".to_owned(), None)
            }
            PushSendOutcome::Rejected { status } => {
                ("rejected", format!("provider_http_{status}"), None)
            }
            PushSendOutcome::NotSent { reason } => ("not_sent", reason, None),
            PushSendOutcome::Unknown { reason } => ("unknown", reason, None),
            PushSendOutcome::Retry { after } => {
                let delay = after
                    .unwrap_or(Duration::from_secs(
                        2u64.saturating_pow(attempts.min(8)).min(60),
                    ))
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                let next = now.saturating_add(delay.max(1000));
                if attempts < 6 && next < expires {
                    ("queued", "provider_retry_scheduled".to_owned(), Some(next))
                } else {
                    ("expired", "retry_budget_exhausted".to_owned(), None)
                }
            }
        };
        tx.execute("UPDATE push_jobs SET state=?2,result=?3,due=COALESCE(?4,due),payload=CASE WHEN ?2='queued' THEN payload ELSE x'' END WHERE id=?1",params![id,state,result,retry])?;
        tx.commit()?;
        self.push_expire(now)?;
        Ok(())
    }
    pub(super) fn push_interrupt(&mut self) -> Result<()> {
        self.connection.execute("UPDATE push_jobs SET state='unknown',result='shutdown_after_dispatch',payload=x'' WHERE state='dispatching'",[])?;
        Ok(())
    }
    pub(super) fn push_live(&self, id: &str, generation: u64) -> Result<bool> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM push_destinations WHERE id=?1 AND generation=?2 AND state IN ('active','pending'))",params![id,generation],|r|r.get(0))?)
    }
}
