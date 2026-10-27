//! mongod's logical session cache and transaction reaper (RM-224), and the
//! session a retryable `bulkWrite` logs its writes under, over real
//! WiredTiger. Every expectation is what mongod 8.2.11 did on a one-member
//! replica set started with `TransactionRecordMinimumLifetimeMinutes=0`
//! (2026-10-27); `tests/mongod_live/test_logical_sessions.py` compares the
//! two servers live.

mod common;

use std::sync::Arc;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_transactions, with_wt};
use secantus_commands::params::ServerParams;
use secantus_commands::CommandContext;

fn lsid(n: u8) -> Document {
    doc! {"id": bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![n; 16] }}
}

/// A one-member set's primary with test commands on, as `bind` sets one up.
fn primary(c: &mut CommandContext) -> Arc<ServerParams> {
    with_transactions(c);
    c.replica_set_name = Some("rs0".into());
    c.server_address = Some(("127.0.0.1".into(), 27999));
    c.failpoints = Some(Arc::new(
        secantus_commands::failpoints::FailPointRegistry::new(),
    ));
    let params = Arc::new(ServerParams::new());
    params.set_startup("TransactionRecordMinimumLifetimeMinutes", Bson::Int32(0));
    c.server_params = Some(params.clone());
    let storage = c.storage.clone().unwrap();
    secantus_commands::cluster_admin::record_sessions_collection(storage.as_ref()).unwrap();
    secantus_commands::cluster_admin::record_session_state_collections(storage.as_ref()).unwrap();
    params
}

fn on(db: &str, cmd: &Document, c: &mut CommandContext) -> Document {
    let previous = std::mem::replace(&mut c.db_name, db.into());
    let mut reply = dispatch_full(cmd, c);
    c.db_name = previous;
    reply.remove("$clusterTime");
    reply.remove("operationTime");
    reply
}

fn batch(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .and_then(|c| c.get_array("firstBatch"))
        .unwrap_or_else(|_| panic!("{reply}"))
        .iter()
        .filter_map(|d| d.as_document().cloned())
        .collect()
}

/// The session id byte (`lsid(n)`'s `n`) of a document's `_id.id`.
fn session_byte(id: &Bson) -> u8 {
    match id {
        Bson::Document(d) => match d.get("id") {
            Some(Bson::Binary(b)) => b.bytes[0],
            _ => panic!("{d}"),
        },
        other => panic!("{other}"),
    }
}

fn sessions_in(c: &mut CommandContext, coll: &str) -> Vec<u8> {
    let mut out: Vec<u8> = batch(&on("config", &doc! {"find": coll}, c))
        .iter()
        .map(|d| session_byte(d.get("_id").unwrap()))
        .collect();
    out.sort_unstable();
    out
}

/// `(op, ns, session)` of each entry after `after` on a `config` session
/// collection.
fn config_entries(c: &mut CommandContext, after: bson::Timestamp) -> Vec<(String, String, u8)> {
    batch(&on(
        "local",
        &doc! {"find": "oplog.rs", "filter": {
            "ts": {"$gt": after},
            "ns": {"$in": ["config.system.sessions", "config.transactions", "config.image_collection"]},
        }},
        c,
    ))
    .iter()
    .map(|e| {
        let key = e
            .get_document("o2")
            .ok()
            .and_then(|o2| o2.get("_id"))
            .or_else(|| e.get_document("o").ok().and_then(|o| o.get("_id")))
            .unwrap();
        (
            e.get_str("op").unwrap().to_string(),
            e.get_str("ns").unwrap().to_string(),
            session_byte(key),
        )
    })
    .collect()
}

fn last_ts(c: &mut CommandContext) -> bson::Timestamp {
    let newest = batch(&on(
        "local",
        &doc! {"find": "oplog.rs", "sort": {"$natural": -1}, "limit": 1},
        c,
    ));
    newest[0].get_timestamp("ts").unwrap()
}

#[test]
fn the_cache_refreshes_records_and_the_reaper_deletes_expired_sessions_state() {
    with_wt(|c| {
        let params = primary(c);
        on(
            "t",
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}]},
            c,
        );
        let start = last_ts(c);
        // A retryable insert, a retryable findAndModify, and a read.
        on(
            "t",
            &doc! {"insert": "c", "documents": [{"_id": 3}], "lsid": lsid(1), "txnNumber": 1_i64},
            c,
        );
        let fam = doc! {
            "findAndModify": "c", "query": {"_id": 1}, "update": {"$inc": {"x": 1}},
            "lsid": lsid(2), "txnNumber": 1_i64,
        };
        on("t", &fam, c);
        on("t", &doc! {"find": "c", "lsid": lsid(3)}, c);
        // The session state is kept beside the oplog, not in it.
        assert_eq!(sessions_in(c, "transactions"), [1, 2]);
        assert_eq!(sessions_in(c, "image_collection"), [2]);
        let row = &batch(&on("config", &doc! {"find": "transactions"}, c))[0];
        assert_eq!(
            row.keys().collect::<Vec<_>>(),
            ["_id", "txnNum", "lastWriteOpTime", "lastWriteDate"]
        );
        assert_eq!(row.get_i64("txnNum"), Ok(1));
        assert_eq!(
            row.get_document("lastWriteOpTime").unwrap().get_i64("t"),
            Ok(1)
        );
        assert_eq!(config_entries(c, start), []);

        // The refresh writes a record for each session used since the last.
        let start = last_ts(c);
        let reply = on("admin", &doc! {"refreshLogicalSessionCacheNow": 1}, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        assert_eq!(sessions_in(c, "system.sessions"), [1, 2, 3]);
        let mut entries = config_entries(c, start);
        entries.sort();
        assert_eq!(
            entries,
            [1, 2, 3].map(|n| ("i".to_string(), "config.system.sessions".to_string(), n))
        );
        assert!(params.sessions().is_empty());
        // A record that is there is updated.
        on("t", &doc! {"find": "c", "lsid": lsid(3)}, c);
        let start = last_ts(c);
        on("admin", &doc! {"refreshLogicalSessionCacheNow": 1}, c);
        assert_eq!(
            config_entries(c, start),
            [("u".to_string(), "config.system.sessions".to_string(), 3)]
        );

        // A session with a record is not expired.
        let start = last_ts(c);
        on("admin", &doc! {"reapLogicalSessionCacheNow": 1}, c);
        assert_eq!(config_entries(c, start), []);
        assert_eq!(sessions_in(c, "transactions"), [1, 2]);

        // Ended sessions' records go at the next refresh ...
        let reply = on("admin", &doc! {"endSessions": [lsid(1), lsid(2)]}, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        let start = last_ts(c);
        on("admin", &doc! {"refreshLogicalSessionCacheNow": 1}, c);
        let mut entries = config_entries(c, start);
        entries.sort();
        assert_eq!(
            entries,
            [1, 2].map(|n| ("d".to_string(), "config.system.sessions".to_string(), n))
        );
        // ... and the reaper then deletes their images and rows, the images
        // first, as logged deletes.
        let start = last_ts(c);
        on("admin", &doc! {"reapLogicalSessionCacheNow": 1}, c);
        assert_eq!(
            config_entries(c, start),
            [
                ("d".to_string(), "config.image_collection".to_string(), 2),
                ("d".to_string(), "config.transactions".to_string(), 1),
                ("d".to_string(), "config.transactions".to_string(), 2),
            ]
        );
        assert_eq!(sessions_in(c, "transactions"), Vec::<u8>::new());
        assert_eq!(sessions_in(c, "image_collection"), Vec::<u8>::new());

        // A retry of a reaped write runs it again.
        let again = on("t", &fam, c);
        assert_eq!(
            again.get_document("value").unwrap().get_i32("x"),
            Ok(2),
            "{again}"
        );
        let status = on("admin", &doc! {"serverStatus": 1}, c);
        let cache = status.get_document("logicalSessionRecordCache").unwrap();
        assert_eq!(cache.get_i32("sessionsCollectionJobCount"), Ok(3));
        assert_eq!(
            cache.get_i32("lastSessionsCollectionJobEntriesEnded"),
            Ok(2)
        );
        assert_eq!(cache.get_i32("transactionReaperJobCount"), Ok(2));
        assert_eq!(
            cache.get_i32("lastTransactionReaperJobEntriesCleanedUp"),
            Ok(2)
        );
    });
}

#[test]
fn a_committed_transaction_leaves_its_row() {
    with_wt(|c| {
        primary(c);
        on("t", &doc! {"create": "c"}, c);
        let mut stmt = doc! {"insert": "c", "documents": [{"_id": 1}], "lsid": lsid(4)};
        stmt.insert("txnNumber", 5_i64);
        stmt.insert("autocommit", false);
        stmt.insert("startTransaction", true);
        on("t", &stmt, c);
        assert_eq!(sessions_in(c, "transactions"), Vec::<u8>::new());
        let commit = doc! {
            "commitTransaction": 1, "lsid": lsid(4), "txnNumber": 5_i64, "autocommit": false,
        };
        let reply = on("admin", &commit, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        let rows = batch(&on("config", &doc! {"find": "transactions"}, c));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_i64("txnNum"), Ok(5));
        assert_eq!(rows[0].get_str("state"), Ok("committed"));
        // Its `lastWriteOpTime` is the time of the one `applyOps` entry the
        // oplog shows for it.
        let entry = &batch(&on(
            "local",
            &doc! {"find": "oplog.rs", "filter": {"op": "c", "txnNumber": 5_i64}},
            c,
        ))[0];
        assert_eq!(
            rows[0]
                .get_document("lastWriteOpTime")
                .unwrap()
                .get_timestamp("ts"),
            entry.get_timestamp("ts")
        );
    });
}

#[test]
fn the_test_commands_exist_only_with_test_commands_on() {
    with_wt(|c| {
        for name in [
            "refreshLogicalSessionCacheNow",
            "reapLogicalSessionCacheNow",
        ] {
            let reply = on("admin", &doc! {name: 1}, c);
            assert_eq!(reply.get_i32("code"), Ok(59), "{reply}");
        }
        primary(c);
        // mongod 8.2.11 takes any value, on any database, and other fields.
        for (db, cmd) in [
            ("t", doc! {"refreshLogicalSessionCacheNow": "x", "zz": 1}),
            ("admin", doc! {"reapLogicalSessionCacheNow": false}),
        ] {
            let reply = on(db, &cmd, c);
            assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        }
    });
}

/// A retryable `bulkWrite` logs each op under its statement id, and its
/// consecutive inserts into one namespace as one `applyOps`.
#[test]
fn a_retryable_bulk_write_logs_its_session() {
    with_wt(|c| {
        primary(c);
        on("t", &doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let start = last_ts(c);
        let reply = on(
            "admin",
            &doc! {
                "bulkWrite": 1,
                "ops": [
                    {"insert": 0, "document": {"_id": 10}},
                    {"insert": 0, "document": {"_id": 11}},
                    {"update": 0, "filter": {"_id": 1}, "updateMods": {"$set": {"y": 1}}},
                    {"insert": 0, "document": {"_id": 12}},
                ],
                "nsInfo": [{"ns": "t.c"}],
                "lsid": lsid(5),
                "txnNumber": 1_i64,
            },
            c,
        );
        assert_eq!(reply.get_i32("nInserted"), Ok(3), "{reply}");
        let entries = batch(&on(
            "local",
            &doc! {"find": "oplog.rs", "filter": {"ts": {"$gt": start}}},
            c,
        ));
        // (op, stmtId, the applyOps' stmtIds, multiOpType) of each entry.
        type Shape = (String, Option<i32>, Vec<i32>, Option<i32>);
        let shape: Vec<Shape> = entries
            .iter()
            .map(|e| {
                (
                    e.get_str("op").unwrap().to_string(),
                    e.get_i32("stmtId").ok(),
                    e.get_document("o")
                        .ok()
                        .and_then(|o| o.get_array("applyOps").ok())
                        .map(|ops| {
                            ops.iter()
                                .map(|op| op.as_document().unwrap().get_i32("stmtId").unwrap())
                                .collect()
                        })
                        .unwrap_or_default(),
                    e.get_i32("multiOpType").ok(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("c".to_string(), None, vec![0, 1], Some(1)),
                ("u".to_string(), Some(2), vec![], None),
                ("i".to_string(), Some(3), vec![], None),
            ]
        );
        for e in &entries {
            assert_eq!(
                e.keys().take(2).collect::<Vec<_>>(),
                ["lsid", "txnNumber"],
                "{e}"
            );
        }
    });
}

/// A retryable write's `operationTime` is the time of its own entry, though
/// keeping its session's row moves the clock on: a change stream started
/// there sees the write (mongod 8.2.11).
#[test]
fn a_retryable_write_answers_the_time_of_its_entry() {
    with_wt(|c| {
        primary(c);
        c.db_name = "t".into();
        for cmd in [
            doc! {"insert": "c", "documents": [{"_id": 1}], "lsid": lsid(6), "txnNumber": 1_i64},
            doc! {
                "findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"y": 1}},
                "lsid": lsid(6), "txnNumber": 2_i64,
            },
        ] {
            let reply = dispatch_full(&cmd, c);
            let newest = batch(&on(
                "local",
                &doc! {
                    "find": "oplog.rs", "filter": {"ns": "t.c"},
                    "sort": {"$natural": -1}, "limit": 1,
                },
                c,
            ));
            assert_eq!(
                reply.get_timestamp("operationTime"),
                newest[0].get_timestamp("ts"),
                "{reply}"
            );
            let keys: Vec<&String> = reply.keys().collect();
            assert_eq!(&keys[keys.len() - 2..], ["$clusterTime", "operationTime"]);
        }
    });
}
