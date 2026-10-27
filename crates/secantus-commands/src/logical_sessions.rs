//! mongod's logical session cache and its transaction reaper (RM-224).
//!
//! Every command that names a session puts it in the cache's set of active
//! sessions (`params::ServerParams::note_session`). Every
//! `logicalSessionRefreshMillis` (five minutes; a startup parameter) mongod
//! runs two jobs, which `refreshLogicalSessionCacheNow` and
//! `reapLogicalSessionCacheNow` run at once when test commands are on:
//!
//! * the **refresh** writes each active session's `lastUse` to
//!   `config.system.sessions` (an upsert: an `i` entry for a new record, a
//!   `u` with the `lastUse` diff for one already there), deletes the records
//!   of the sessions `endSessions` ended, closes their cursors, and starts
//!   the active set again. A record nobody refreshes is expired by the
//!   collection's TTL index `lsidTTLIndex` after
//!   `localLogicalSessionTimeoutMinutes` (30; startup only), which the TTL
//!   monitor runs like any other.
//! * the **reaper** looks at the `config.transactions` rows written more than
//!   `TransactionRecordMinimumLifetimeMinutes` (30; startup only) ago, and
//!   for each whose session has no record left in `config.system.sessions`
//!   deletes the session's retry image from `config.image_collection` and
//!   then its row -- logged deletes, all the images first -- and forgets the
//!   session's retryable writes, so a retry of one runs it again.
//!
//! Measured against mongod 8.2.11 on a one-member replica set (2026-10-27):
//! the entries, their order, the documents, and `serverStatus`'s
//! `logicalSessionRecordCache`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

use bson::{doc, Bson, Document};

use crate::cursors::CursorRegistry;
use crate::params::ServerParams;
use crate::storage::{Storage, StorageError};
use crate::transactions::TransactionRegistry;

/// mongod's defaults (8.2.11).
pub const DEFAULT_REFRESH_MILLIS: i64 = 300_000;
pub const DEFAULT_TIMEOUT_MINUTES: i64 = 30;
pub const DEFAULT_RECORD_LIFETIME_MINUTES: i64 = 30;

const CONFIG: &str = "config";
const SESSIONS: &str = "system.sessions";
const TRANSACTIONS: &str = "transactions";
const IMAGES: &str = "image_collection";

/// The logical session cache's state beside the active sessions: the
/// sessions ended since the last refresh, and what its jobs last did.
#[derive(Default)]
pub struct LogicalSessionCache {
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    /// Ended sessions, by id, with the `uid` of the user who ended them.
    ending: BTreeMap<Vec<u8>, Vec<u8>>,
    refresh: Option<JobStats>,
    refreshes: i32,
    reap: Option<JobStats>,
    reaps: i32,
}

/// One run of a job, as `serverStatus` reports it.
#[derive(Clone, Copy)]
struct JobStats {
    started: bson::DateTime,
    finished: bson::DateTime,
    millis: i32,
    /// The refresh: (records refreshed, records ended, cursors closed); the
    /// reaper: (rows deleted, 0, 0).
    counts: (i32, i32, i32),
}

impl LogicalSessionCache {
    fn locked(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `endSessions`: the sessions leave the active set now, and the next
    /// refresh deletes their records.
    pub fn end(&self, ids: Vec<Vec<u8>>, uid: &[u8]) {
        let mut state = self.locked();
        for id in ids {
            state.ending.insert(id, uid.to_vec());
        }
    }

    /// `serverStatus.logicalSessionRecordCache`, in mongod's order.
    pub fn server_status(&self, active: usize, catalog: usize) -> Document {
        let state = self.locked();
        let zero = bson::DateTime::from_millis(0);
        let refresh = state.refresh.unwrap_or(JobStats {
            started: zero,
            finished: zero,
            millis: 0,
            counts: (0, 0, 0),
        });
        let reap = state.reap.unwrap_or(JobStats {
            started: zero,
            finished: zero,
            millis: 0,
            counts: (0, 0, 0),
        });
        doc! {
            "activeSessionsCount": active as i32,
            "sessionsCollectionJobCount": state.refreshes,
            "lastSessionsCollectionJobDurationMillis": refresh.millis,
            "lastSessionsCollectionJobDurationMillisUpdatedTimestamp": refresh.finished,
            "lastSessionsCollectionJobTimestamp": refresh.started,
            "lastSessionsCollectionJobEntriesRefreshed": refresh.counts.0,
            "lastSessionsCollectionJobEntriesRefreshedUpdatedTimestamp": refresh.finished,
            "lastSessionsCollectionJobEntriesEnded": refresh.counts.1,
            "lastSessionsCollectionJobEntriesEndedUpdatedTimestamp": refresh.finished,
            "lastSessionsCollectionJobCursorsClosed": refresh.counts.2,
            "lastSessionsCollectionJobCursorsClosedUpdatedTimestamp": refresh.finished,
            "transactionReaperJobCount": state.reaps,
            "lastTransactionReaperJobDurationMillis": reap.millis,
            "lastTransactionReaperJobDurationUpdatedTimestamp": reap.finished,
            "lastTransactionReaperJobTimestamp": reap.started,
            "lastTransactionReaperJobEntriesCleanedUp": reap.counts.0,
            "lastTransactionReaperJobEntriesCleanedUpUpdatedTimestamp": reap.finished,
            "sessionCatalogSize": catalog as i32,
        }
    }
}

/// The logical session id a record is kept under: `{id, uid}`.
fn record_id(id: &[u8], uid: &[u8]) -> Document {
    doc! {
        "id": bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: id.to_vec() },
        "uid": bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: uid.to_vec() },
    }
}

fn session_of(record_id: &Document) -> Option<Vec<u8>> {
    match record_id.get("id") {
        Some(Bson::Binary(b)) => Some(b.bytes.clone()),
        _ => None,
    }
}

fn elapsed_millis(since: std::time::Instant) -> i32 {
    i32::try_from(since.elapsed().as_millis()).unwrap_or(i32::MAX)
}

/// How long a session record lives without a refresh, in seconds: the TTL
/// of `lsidTTLIndex`.
pub fn session_timeout_seconds(params: &ServerParams) -> i64 {
    params.number("localLogicalSessionTimeoutMinutes", DEFAULT_TIMEOUT_MINUTES) * 60
}

/// The refresh job: write the active sessions' records, delete the ended
/// sessions' records, and close the ended (or expired) sessions' cursors.
pub fn refresh(
    storage: &dyn Storage,
    params: &ServerParams,
    cursors: Option<&CursorRegistry>,
) -> Result<(), StorageError> {
    let started = bson::DateTime::now();
    let clock = std::time::Instant::now();
    let cache = params.logical_sessions();
    // mongod sets the collection up again when it is missing.
    crate::cluster_admin::record_sessions_collection_ttl(storage, session_timeout_seconds(params))?;
    let ending = std::mem::take(&mut cache.locked().ending);
    let mut active = params.take_sessions();
    // A session with an open cursor is in use, and is refreshed too.
    if let Some(cursors) = cursors {
        for (id, uid) in cursors.sessions() {
            active.entry(id).or_insert((started, uid));
        }
    }
    for id in ending.keys() {
        active.remove(id);
    }
    let now = bson::DateTime::now();
    let mut refreshed = 0i32;
    for (id, (_, uid)) in &active {
        storage.update_matching(
            CONFIG,
            SESSIONS,
            &doc! {"_id": record_id(id, uid)},
            &doc! {"$set": {"lastUse": now}},
            false,
            true,
        )?;
        refreshed += 1;
    }
    for (id, uid) in &ending {
        storage.delete_matching(CONFIG, SESSIONS, &doc! {"_id": record_id(id, uid)}, 1)?;
    }
    // The cursors of a session that ended, or whose record has expired, are
    // closed.
    let mut closed = 0i32;
    if let Some(cursors) = cursors {
        let mut gone: HashSet<Vec<u8>> = ending.keys().cloned().collect();
        let open: Vec<(Vec<u8>, Vec<u8>)> = cursors.sessions();
        if !open.is_empty() {
            let ids: Vec<Bson> = open
                .iter()
                .map(|(id, uid)| Bson::Document(record_id(id, uid)))
                .collect();
            let present = present_records(storage, ids)?;
            for (id, _) in open {
                if !present.contains(&id) {
                    gone.insert(id);
                }
            }
        }
        closed = cursors.kill_sessions(&gone) as i32;
    }
    let mut state = cache.locked();
    state.refreshes += 1;
    state.refresh = Some(JobStats {
        started,
        finished: bson::DateTime::now(),
        millis: elapsed_millis(clock),
        counts: (refreshed, ending.len() as i32, closed),
    });
    Ok(())
}

/// The ids of the sessions among `ids` (record ids) that have a record in
/// `config.system.sessions`.
fn present_records(
    storage: &dyn Storage,
    ids: Vec<Bson>,
) -> Result<HashSet<Vec<u8>>, StorageError> {
    let mut present = HashSet::new();
    for chunk in ids.chunks(1000) {
        let filter = doc! {"_id": {"$in": chunk.to_vec()}};
        for bytes in storage.find(CONFIG, SESSIONS, &filter, None, None)? {
            let record = crate::bsondoc::from_bytes(&bytes)
                .map_err(|e| StorageError::Internal(format!("config.system.sessions: {e}")))?;
            if let Some(id) = record.get_document("_id").ok().and_then(session_of) {
                present.insert(id);
            }
        }
    }
    Ok(present)
}

/// The transaction reaper: delete the images and `config.transactions` rows
/// of the sessions that have expired.
pub fn reap(
    storage: &dyn Storage,
    params: &ServerParams,
    transactions: Option<&TransactionRegistry>,
) -> Result<(), StorageError> {
    let started = bson::DateTime::now();
    let clock = std::time::Instant::now();
    let lifetime = params.number(
        "TransactionRecordMinimumLifetimeMinutes",
        DEFAULT_RECORD_LIFETIME_MINUTES,
    );
    let cutoff = bson::DateTime::from_millis(
        started
            .timestamp_millis()
            .saturating_sub(lifetime.saturating_mul(60_000)),
    );
    let mut candidates: Vec<Document> = Vec::new();
    if storage.collection_exists(CONFIG, TRANSACTIONS)? {
        let filter = doc! {"lastWriteDate": {"$lt": cutoff}};
        for bytes in storage.find(CONFIG, TRANSACTIONS, &filter, None, None)? {
            let row = crate::bsondoc::from_bytes(&bytes)
                .map_err(|e| StorageError::Internal(format!("config.transactions: {e}")))?;
            if let Ok(id) = row.get_document("_id") {
                candidates.push(id.clone());
            }
        }
    }
    let present = if candidates.is_empty() {
        HashSet::new()
    } else {
        present_records(
            storage,
            candidates.iter().cloned().map(Bson::Document).collect(),
        )?
    };
    // A session is expired when it has no record, whether or not it was
    // used since the last refresh: mongod 8.2.11 reaps the row of a session
    // no refresh has recorded yet.
    let expired: Vec<Document> = candidates
        .into_iter()
        .filter(|id| session_of(id).is_some_and(|s| !present.contains(&s)))
        .collect();
    if !expired.is_empty() && storage.collection_exists(CONFIG, IMAGES)? {
        for id in &expired {
            storage.delete_matching(CONFIG, IMAGES, &doc! {"_id": id.clone()}, 1)?;
        }
    }
    for id in &expired {
        storage.delete_matching(CONFIG, TRANSACTIONS, &doc! {"_id": id.clone()}, 1)?;
        if let (Some(registry), Some(session)) = (transactions, session_of(id)) {
            registry.forget_session(&session);
        }
    }
    let cache = params.logical_sessions();
    let mut state = cache.locked();
    state.reaps += 1;
    state.reap = Some(JobStats {
        started,
        finished: bson::DateTime::now(),
        millis: elapsed_millis(clock),
        counts: (expired.len() as i32, 0, 0),
    });
    Ok(())
}

/// Run both jobs when they are due: the server calls this from its
/// periodic loop, every second or so. `last` is when they last ran.
pub fn run_if_due(
    storage: &dyn Storage,
    params: &ServerParams,
    cursors: Option<&CursorRegistry>,
    transactions: Option<&TransactionRegistry>,
    last: &mut std::time::Instant,
) -> Result<(), StorageError> {
    if params.flag("disableLogicalSessionCacheRefresh", false) {
        return Ok(());
    }
    let every = params
        .number("logicalSessionRefreshMillis", DEFAULT_REFRESH_MILLIS)
        .max(1);
    if last.elapsed() < std::time::Duration::from_millis(every as u64) {
        return Ok(());
    }
    *last = std::time::Instant::now();
    refresh(storage, params, cursors)?;
    reap(storage, params, transactions)
}
