use super::{NotificationObservation, now_ms};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{
    Notification, NotificationCursor, NotificationHealth, NotificationKind, NotificationPage,
    NotificationPolicy, NotificationQuery, NotificationScope, NotificationSource,
};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, Transaction, params};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

pub(super) const MAX_RECORDS: usize = 4096;
pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 8192;
const PAGE_BYTES: usize = 128 * 1024;
pub(super) const MAX_EPISODES: usize = 4096;
pub(super) const MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const APPLICATION_ID: i64 = 0x424b4e31;

struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[derive(Debug)]
struct UnsafeStorage;
impl std::fmt::Display for UnsafeStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("notification storage must be private, current-user-owned and not linked")
    }
}
impl std::error::Error for UnsafeStorage {}

#[derive(Debug)]
struct EpisodeCapacity;
impl std::fmt::Display for EpisodeCapacity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("notification episode capacity exceeded")
    }
}
impl std::error::Error for EpisodeCapacity {}

pub(super) struct Unavailable {
    _directory: File,
    _lease: Arc<Lease>,
}
pub(super) enum Opened {
    Ready(Store),
    Unavailable(Unavailable),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Working,
    Blocked,
    Error,
    Idle,
    Done,
}
struct Identity {
    session_instance_id: String,
    process: bkpr_protocol::ProcessIdentity,
    harness: bkpr_protocol::HarnessKind,
    launch_id: Option<String>,
    binding_generation: Option<u64>,
}
impl Identity {
    fn matches(&self, source: &NotificationSource) -> bool {
        self.session_instance_id == source.session.session_instance_id
            && self.process == source.process
            && self.harness == source.harness
            && self.launch_id == source.launch_id
            && self.binding_generation == source.binding_generation
    }
    fn new(source: &NotificationSource) -> Self {
        Self {
            session_instance_id: source.session.session_instance_id.clone(),
            process: source.process.clone(),
            harness: source.harness,
            launch_id: source.launch_id.clone(),
            binding_generation: source.binding_generation,
        }
    }
}
struct Episode {
    identity: Identity,
    state: State,
    seen: u8,
}

pub(super) struct Store {
    // Connection closes before exclusive authority is released.
    connection: Connection,
    _directory: File,
    _lease: Arc<Lease>,
    store_id: String,
    policies: Vec<NotificationPolicy>,
    dropped: u64,
    failed: bool,
    episodes: BTreeMap<String, BTreeMap<String, Episode>>,
    episode_count: usize,
    next_expiry: Option<Option<u64>>,
}
impl Store {
    pub(super) fn open(root: &Path) -> Result<Opened> {
        // Reuse registry ownership/mode policy, then reject symlink traversal and
        // retain directory/file authority for the entire SQLite connection lifetime.
        let parent = root
            .parent()
            .context("notification root requires a parent")?;
        private_path(parent)?;
        super::super::registry::private_directory(root)?;
        let directory = private_path(root)?;
        let lease = open_private(&root.join("store.lock"), true)?;
        ensure!(
            lease.metadata()?.nlink() == 1,
            "notification lease must have one link"
        );
        FileExt::try_lock_exclusive(&lease).context("notification storage already owned")?;
        let lease = Arc::new(Lease(lease));
        let authority = Unavailable {
            _directory: directory.try_clone()?,
            _lease: lease.clone(),
        };
        let initialized = (|| {
            for suffix in ["", "-journal", "-wal", "-shm"] {
                let path = root.join(format!("notifications.sqlite3{suffix}"));
                if path.try_exists()? || std::fs::symlink_metadata(&path).is_ok() {
                    let file = open_private(&path, false)?;
                    ensure!(
                        file.metadata()?.len() <= MAX_FILE_BYTES,
                        "notification storage exceeds physical bound"
                    );
                }
            }
            let db_path = root.join("notifications.sqlite3");
            let file = open_private(&db_path, true)?;
            Self::initialize(directory, lease, &db_path, file.metadata()?.len() == 0)
        })();
        match initialized {
            Ok(store) => Ok(Opened::Ready(store)),
            Err(error) if error.downcast_ref::<UnsafeStorage>().is_some() => Err(error),
            Err(_) => Ok(Opened::Unavailable(authority)),
        }
    }
    fn initialize(directory: File, lease: Arc<Lease>, db_path: &Path, empty: bool) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::from_bits_retain(rusqlite::ffi::SQLITE_OPEN_NOFOLLOW);
        let connection = Connection::open_with_flags(db_path, flags)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let application: i64 =
            connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        ensure!(
            (empty && application == 0) || application == APPLICATION_ID,
            "foreign notification database"
        );
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(
            version == 0 || version == 1,
            "unsupported notification storage version"
        );
        let integrity: String =
            connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        ensure!(
            integrity == "ok",
            "notification database integrity check failed"
        );
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             PRAGMA mmap_size=0;
             PRAGMA cache_size=-512;
             PRAGMA temp_store=MEMORY;
             PRAGMA max_page_count=8192;
             PRAGMA journal_size_limit=33554432;
             PRAGMA auto_vacuum=FULL;",
        )?;
        if empty {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute_batch(
                "CREATE TABLE metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), store_id TEXT NOT NULL, sequence INTEGER NOT NULL, pruned INTEGER NOT NULL, retained INTEGER NOT NULL, bytes INTEGER NOT NULL, dropped INTEGER NOT NULL);
                 CREATE TABLE notifications (sequence INTEGER PRIMARY KEY, expires INTEGER NOT NULL, bytes INTEGER NOT NULL, record TEXT NOT NULL);
                 CREATE INDEX notifications_expiry ON notifications(expires);
                 CREATE TRIGGER notification_insert AFTER INSERT ON notifications BEGIN UPDATE metadata SET retained=retained+1,bytes=bytes+new.bytes; END;
                 CREATE TRIGGER notification_delete AFTER DELETE ON notifications BEGIN UPDATE metadata SET retained=retained-1,bytes=bytes-old.bytes; END;
                 CREATE TABLE policies (scope TEXT PRIMARY KEY, rule TEXT NOT NULL);
                 PRAGMA application_id=1112231473;
                 PRAGMA user_version=1;"
            )?;
            transaction.execute(
                "INSERT INTO metadata VALUES (1, ?1, 0, 0, 0, 0, 0)",
                [Uuid::new_v4().to_string()],
            )?;
            let policy = NotificationPolicy::default();
            transaction.execute(
                "INSERT INTO policies VALUES (?1, ?2)",
                params![
                    serde_json::to_string(&policy.scope)?,
                    serde_json::to_string(&policy)?
                ],
            )?;
            transaction.commit()?;
        }
        let store_id: String = connection.query_row(
            "SELECT store_id FROM metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            Uuid::parse_str(&store_id).is_ok(),
            "invalid notification store identity"
        );
        let mut policies = Vec::new();
        {
            let mut statement =
                connection.prepare("SELECT rule FROM policies ORDER BY scope LIMIT 65")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let policy: NotificationPolicy = serde_json::from_str(&row?)?;
                policy.validate()?;
                policies.push(policy);
            }
        }
        ensure!(
            policies.len() <= 64,
            "too many persisted notification policies"
        );
        ensure!(
            !policies.is_empty(),
            "notification policy storage is incomplete"
        );
        for (index, policy) in policies.iter().enumerate() {
            ensure!(
                !policies[..index]
                    .iter()
                    .any(|other| ambiguous(&policy.scope, &other.scope)),
                "ambiguous persisted notification policy"
            );
        }
        let dropped: u64 = connection.query_row(
            "SELECT dropped FROM metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let mut store = Self {
            connection,
            _directory: directory,
            _lease: lease,
            store_id,
            policies,
            dropped,
            failed: false,
            episodes: BTreeMap::new(),
            episode_count: 0,
            next_expiry: None,
        };
        let (latest, pruned, retained, bytes): (i64, i64, i64, i64) = store.connection.query_row(
            "SELECT sequence,pruned,retained,bytes FROM metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let actual: (i64, i64) = store.connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM notifications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            latest >= pruned
                && pruned >= 0
                && retained == latest - pruned
                && bytes >= 0
                && actual == (retained, bytes),
            "notification metadata integrity failed"
        );
        // Validate retained bounded payloads before exposing any cursor or policy.
        {
            let mut statement = store
                .connection
                .prepare("SELECT sequence,bytes,record FROM notifications ORDER BY sequence")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, usize>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut expected = pruned;
            for row in rows {
                let (sequence, bytes, record) = row?;
                expected += 1;
                let notification: Notification = serde_json::from_str(&record)?;
                notification.validate()?;
                ensure!(
                    record.len() <= MAX_RECORD_BYTES
                        && bytes == record.len()
                        && sequence == expected
                        && notification.sequence == sequence as u64,
                    "invalid persisted notification sequence or size"
                );
            }
            ensure!(
                expected == latest,
                "incomplete persisted notification sequence"
            );
        }
        store.prune(now_ms())?;
        Ok(store)
    }
    pub(super) fn retained(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT retained FROM metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )?)
    }
    pub(super) fn failed(&self) -> bool {
        self.failed
    }
    pub(super) fn policies(&self) -> &[NotificationPolicy] {
        &self.policies
    }
    pub(super) fn dropped(&self) -> u64 {
        self.dropped
    }
    pub(super) fn sync_dropped(&mut self, dropped: u64) -> Result<()> {
        let dropped = dropped.min(i64::MAX as u64);
        if dropped <= self.dropped {
            return Ok(());
        }
        if let Err(error) = self.connection.execute(
            "UPDATE metadata SET dropped=?1 WHERE singleton=1",
            [dropped as i64],
        ) {
            self.failed = true;
            return Err(error.into());
        }
        self.dropped = dropped;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn fail_writes(&self) {
        self.connection
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
    }
    #[cfg(test)]
    pub(super) fn connection_for_test(&mut self) -> &mut Connection {
        &mut self.connection
    }
    pub(super) fn set_policy(&mut self, policy: NotificationPolicy) -> Result<()> {
        policy.validate()?;
        let position = self
            .policies
            .iter()
            .position(|rule| rule.scope == policy.scope);
        ensure!(
            position.is_some() || self.policies.len() < 64,
            "notification policy rule limit reached"
        );
        ensure!(
            !self
                .policies
                .iter()
                .any(|rule| ambiguous(&rule.scope, &policy.scope)),
            "ambiguous equal-specificity notification policy"
        );
        let scope = serde_json::to_string(&policy.scope)?;
        let rule = serde_json::to_string(&policy)?;
        let result = self.connection.execute("INSERT INTO policies VALUES (?1,?2) ON CONFLICT(scope) DO UPDATE SET rule=excluded.rule", params![scope, rule]);
        if let Err(error) = result {
            self.failed = true;
            return Err(error.into());
        }
        if let Some(index) = position {
            self.policies[index] = policy;
        } else {
            self.policies.push(policy);
        }
        Ok(())
    }
    pub(super) fn observe(
        &mut self,
        observation: NotificationObservation,
        now: u64,
    ) -> Result<bool> {
        let result = self.observe_inner(observation, now);
        if result
            .as_ref()
            .is_err_and(|error| error.downcast_ref::<EpisodeCapacity>().is_none())
        {
            self.failed = true;
        }
        result
    }
    fn observe_inner(&mut self, observation: NotificationObservation, now: u64) -> Result<bool> {
        let source = &observation.source;
        let previous = self
            .episodes
            .get(&source.session.session_id)
            .and_then(|panes| panes.get(&source.pane_id));
        let prior = previous.filter(|episode| episode.identity.matches(source));
        let Some(state) = observation.state.as_deref() else {
            if prior.is_some() {
                let panes = self.episodes.get_mut(&source.session.session_id).unwrap();
                panes.remove(&source.pane_id);
                self.episode_count -= 1;
                if panes.is_empty() {
                    self.episodes.remove(&source.session.session_id);
                }
            }
            return Ok(false);
        };
        let state = match state {
            "working" => State::Working,
            "blocked" => State::Blocked,
            "error" => State::Error,
            "idle" => State::Idle,
            "done" => State::Done,
            _ => anyhow::bail!("invalid notification observation state"),
        };
        ensure!(
            previous.is_some() || self.episode_count < MAX_EPISODES,
            EpisodeCapacity
        );
        let mut seen = prior.map_or(0, |episode| episode.seen);
        let attention = match observation.kind.as_deref() {
            None => 1,
            Some("question") => 2,
            Some("permission") => 4,
            Some("auth") => 8,
            _ => anyhow::bail!("invalid notification attention category"),
        };
        let candidate = match state {
            State::Working => {
                seen = 0;
                None
            }
            State::Blocked if seen & attention == 0 => {
                seen |= attention;
                Some(NotificationKind::Attention)
            }
            State::Error if seen & 16 == 0 => {
                seen |= 16;
                Some(NotificationKind::Error)
            }
            State::Done if seen & 32 == 0 => {
                seen |= 32;
                Some(NotificationKind::Done)
            }
            State::Idle if prior.is_some_and(|episode| episode.state == State::Working) => {
                Some(NotificationKind::Idle)
            }
            _ => None,
        };
        let enabled = candidate.filter(|kind| {
            self.policies
                .iter()
                .filter(|policy| matches_scope(&policy.scope, source))
                .max_by_key(|policy| specificity(&policy.scope))
                .is_some_and(|policy| policy.kinds.contains(kind))
        });
        if prior.is_some_and(|episode| episode.state == state && episode.seen == seen)
            && enabled.is_none()
        {
            return Ok(false);
        }
        if let Some(kind) = enabled {
            let transaction = self.connection.transaction()?;
            let sequence: i64 = transaction.query_row(
                "SELECT sequence FROM metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            let sequence = sequence
                .checked_add(1)
                .context("notification sequence exhausted")?;
            let notification = Notification {
                sequence: sequence as u64,
                id: Uuid::new_v4().to_string(),
                source: observation.source,
                attention_kind: if kind == NotificationKind::Attention {
                    observation.kind
                } else {
                    None
                },
                kind,
                observed_at_ms: observation.observed_at_ms,
                expires_at_ms: now.saturating_add(MAX_AGE_MS),
            };
            notification.validate()?;
            let record = serde_json::to_string(&notification)?;
            ensure!(
                record.len() <= MAX_RECORD_BYTES,
                "notification record exceeds byte bound"
            );
            transaction.execute(
                "INSERT INTO notifications VALUES (?1,?2,?3,?4)",
                params![
                    sequence,
                    as_i64(notification.expires_at_ms)?,
                    record.len() as i64,
                    record
                ],
            )?;
            transaction.execute(
                "UPDATE metadata SET sequence=?1 WHERE singleton=1",
                [sequence],
            )?;
            prune_transaction(&transaction, now)?;
            transaction.commit()?;
            self.update_episode(&notification.source, state, seen);
            self.next_expiry = None;
            Ok(true)
        } else {
            self.update_episode(&observation.source, state, seen);
            Ok(false)
        }
    }
    fn update_episode(&mut self, source: &NotificationSource, state: State, seen: u8) {
        if !self.episodes.contains_key(&source.session.session_id) {
            self.episodes
                .insert(source.session.session_id.clone(), BTreeMap::new());
        }
        let panes = self.episodes.get_mut(&source.session.session_id).unwrap();
        if let Some(episode) = panes.get_mut(&source.pane_id) {
            if !episode.identity.matches(source) {
                episode.identity = Identity::new(source);
            }
            episode.state = state;
            episode.seen = seen;
        } else {
            panes.insert(
                source.pane_id.clone(),
                Episode {
                    identity: Identity::new(source),
                    state,
                    seen,
                },
            );
            self.episode_count += 1;
        }
    }
    fn prune(&mut self, now: u64) -> Result<()> {
        let transaction = self.connection.transaction()?;
        prune_transaction(&transaction, now)?;
        transaction.commit()?;
        self.next_expiry = None;
        Ok(())
    }
    pub(super) fn next_expiry(&mut self) -> Result<Option<u64>> {
        if let Some(expiry) = self.next_expiry {
            return Ok(expiry);
        }
        let expiry =
            self.connection
                .query_row("SELECT MIN(expires) FROM notifications", [], |row| {
                    row.get(0)
                })?;
        self.next_expiry = Some(expiry);
        Ok(expiry)
    }
    pub(super) fn expire(&mut self, now: u64) -> Result<bool> {
        let result = (|| {
            let before = self.retained()?;
            self.prune(now)?;
            Ok(self.retained()? != before)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(super) fn page(
        &mut self,
        query: &NotificationQuery,
        after: Option<&NotificationCursor>,
        baseline: bool,
        now: u64,
        mut health: NotificationHealth,
    ) -> Result<NotificationPage> {
        // Cursor mistakes are caller errors, not a poisoned storage engine.
        if let Some(cursor) = after {
            ensure!(
                cursor.store_id == self.store_id,
                "notification cursor belongs to another store"
            );
            ensure!(
                cursor.loss_generation <= self.dropped,
                "notification cursor loss generation is in the future"
            );
            let latest: u64 = match self.connection.query_row(
                "SELECT sequence FROM metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            ) {
                Ok(latest) => latest,
                Err(error) => {
                    self.failed = true;
                    return Err(error.into());
                }
            };
            ensure!(
                cursor.sequence <= latest,
                "notification cursor is in the future"
            );
        }
        let result = self.page_inner(query, after, baseline, now, &mut health);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn page_inner(
        &mut self,
        query: &NotificationQuery,
        after: Option<&NotificationCursor>,
        baseline: bool,
        now: u64,
        health: &mut NotificationHealth,
    ) -> Result<NotificationPage> {
        self.prune(now)?;
        let (latest, pruned): (u64, u64) = self.connection.query_row(
            "SELECT sequence,pruned FROM metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        health.retained = self.retained()?;
        health.dropped_observations = health.dropped_observations.max(self.dropped);
        let mut notifications = Vec::new();
        let mut bytes = 0;
        let mut sequence = after.map_or(latest, |cursor| cursor.sequence);
        let gap = after.is_some_and(|cursor| {
            cursor.sequence < pruned || cursor.loss_generation < self.dropped
        });
        let mut has_more = false;
        if !baseline {
            // First inspection selects the newest matching page, while resumed
            // requests scan forwards. Both advance over nonmatching records.
            let sql = if after.is_some() {
                "SELECT sequence,record FROM notifications WHERE sequence>?1 ORDER BY sequence ASC"
            } else {
                "SELECT sequence,record FROM notifications WHERE sequence<=?1 ORDER BY sequence DESC"
            };
            let bound = after.map_or(latest, |cursor| cursor.sequence);
            let mut statement = self.connection.prepare(sql)?;
            let rows = statement.query_map([as_i64(bound)?], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (entry_sequence, record) = row?;
                let notification: Notification = serde_json::from_str(&record)?;
                let matches = matches_scope(&query.scope, &notification.source)
                    && (query.kinds.is_empty() || query.kinds.contains(&notification.kind));
                if matches
                    && (notifications.len() >= usize::from(query.limit)
                        || bytes + record.len() > PAGE_BYTES)
                {
                    has_more = after.is_some();
                    break;
                }
                if after.is_some() {
                    sequence = entry_sequence;
                }
                if matches {
                    bytes += record.len();
                    notifications.push(notification);
                }
            }
            if after.is_none() {
                notifications.reverse();
            }
            if !has_more {
                sequence = latest;
            }
        }
        Ok(NotificationPage {
            notifications,
            cursor: NotificationCursor {
                store_id: self.store_id.clone(),
                sequence,
                loss_generation: self.dropped,
            },
            gap,
            has_more,
            health: health.clone(),
        })
    }
}
fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("notification integer exceeds durable range")
}
fn specificity(scope: &NotificationScope) -> usize {
    usize::from(scope.session_id.is_some())
        + usize::from(scope.workspace_id.is_some())
        + usize::from(scope.pane_id.is_some())
}
fn ambiguous(a: &NotificationScope, b: &NotificationScope) -> bool {
    fn overlap(a: &Option<String>, b: &Option<String>) -> bool {
        a.is_none() || b.is_none() || a == b
    }
    a != b
        && specificity(a) == specificity(b)
        && overlap(&a.session_id, &b.session_id)
        && overlap(&a.workspace_id, &b.workspace_id)
        && overlap(&a.pane_id, &b.pane_id)
}
fn matches_scope(scope: &NotificationScope, source: &NotificationSource) -> bool {
    scope
        .session_id
        .as_ref()
        .is_none_or(|id| id == &source.session.session_id)
        && scope
            .workspace_id
            .as_ref()
            .is_none_or(|id| source.workspace_id.as_ref() == Some(id))
        && scope
            .pane_id
            .as_ref()
            .is_none_or(|id| id == &source.pane_id)
}
fn prune_transaction(transaction: &Transaction<'_>, now: u64) -> Result<()> {
    let expired: Option<i64> = transaction.query_row(
        "SELECT MAX(sequence) FROM notifications WHERE expires<=?1",
        [as_i64(now)?],
        |row| row.get(0),
    )?;
    if let Some(sequence) = expired {
        prune_through(transaction, sequence)?;
    }
    let (mut count, mut bytes): (usize, usize) = transaction.query_row(
        "SELECT retained,bytes FROM metadata WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if count > MAX_RECORDS || bytes > MAX_BYTES {
        let mut statement = transaction
            .prepare("SELECT sequence,bytes FROM notifications ORDER BY sequence ASC")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, usize>(1)?))
        })?;
        let mut cutoff = None;
        for row in rows {
            let (sequence, entry_bytes) = row?;
            count -= 1;
            bytes -= entry_bytes;
            cutoff = Some(sequence);
            if count <= MAX_RECORDS && bytes <= MAX_BYTES {
                break;
            }
        }
        drop(statement);
        if let Some(sequence) = cutoff {
            prune_through(transaction, sequence)?;
        }
    }
    Ok(())
}
fn prune_through(transaction: &Transaction<'_>, sequence: i64) -> Result<()> {
    transaction.execute("DELETE FROM notifications WHERE sequence<=?1", [sequence])?;
    transaction.execute(
        "UPDATE metadata SET pruned=MAX(pruned,?1) WHERE singleton=1",
        [sequence],
    )?;
    Ok(())
}
fn private_path(path: &Path) -> Result<File> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                current.push(name);
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&current)?;
            }
            _ => anyhow::bail!("notification path may not traverse parent components"),
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&absolute)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "notification directory must be private and current-user-owned"
    );
    Ok(file)
}
fn open_private(path: &Path, create: bool) -> Result<File> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            UnsafeStorage
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                anyhow::Error::new(UnsafeStorage)
            } else {
                error.into()
            }
        })?;
    super::super::registry::check_file(&file).map_err(|_| anyhow::Error::new(UnsafeStorage))?;
    ensure!(file.metadata()?.nlink() == 1, UnsafeStorage);
    Ok(file)
}
