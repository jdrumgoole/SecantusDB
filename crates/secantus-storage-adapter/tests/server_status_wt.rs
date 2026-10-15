//! `serverStatus`'s counters over real WiredTiger: each moves as mongod
//! 8.2.11's did for the same operations (measured 2026-10-15, one operation
//! at a time against a one-member replica set, reading which counters moved).

mod common;

use std::sync::Arc;

use bson::{doc, Bson, Document};
use common::{dispatch_admin, dispatch_full, with_transactions, with_wt};
use secantus_commands::topstats::TopStats;
use secantus_commands::CommandContext;

fn status(c: &mut CommandContext) -> Document {
    dispatch_full(&doc! {"serverStatus": 1}, c)
}

/// The integer at a dotted path of the reply, whatever its width.
fn num(reply: &Document, path: &str) -> i64 {
    let mut at = reply;
    let parts: Vec<&str> = path.split('.').collect();
    for p in &parts[..parts.len() - 1] {
        at = at
            .get_document(p)
            .unwrap_or_else(|_| panic!("{path}: {reply}"));
    }
    match at.get(parts[parts.len() - 1]) {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        other => panic!("{path}: {other:?}"),
    }
}

/// How much each of `paths` moved while `run` ran.
fn moved(
    c: &mut CommandContext,
    paths: &[&str],
    run: impl FnOnce(&mut CommandContext),
) -> Vec<i64> {
    let before = status(c);
    run(c);
    let after = status(c);
    paths
        .iter()
        .map(|p| num(&after, p) - num(&before, p))
        .collect()
}

fn counted(c: &mut CommandContext) {
    c.top_stats = Some(Arc::new(TopStats::new()));
}

#[test]
fn the_sections_mongod_reports_are_there() {
    with_wt(|c| {
        counted(c);
        with_transactions(c);
        let reply = status(c);
        let keys: Vec<&str> = reply.keys().map(String::as_str).collect();
        // mongod's order: the sections by name, then `mem` and `metrics`.
        let want = [
            "asserts",
            "connections",
            "extra_info",
            "globalLock",
            "logicalSessionRecordCache",
            "network",
            "opLatencies",
            "opcounters",
            "security",
            "storageEngine",
            "transactions",
            "wiredTiger",
            "mem",
            "metrics",
        ];
        let at: Vec<usize> = want
            .iter()
            .map(|k| {
                keys.iter()
                    .position(|g| g == k)
                    .unwrap_or_else(|| panic!("{k}"))
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
        let metrics = reply.get_document("metrics").unwrap();
        for k in [
            "commands",
            "cursor",
            "document",
            "operation",
            "query",
            "ttl",
        ] {
            assert!(metrics.contains_key(k), "metrics.{k}");
        }
        // Nothing this server does not count stands in as a zero.
        assert!(!reply.contains_key("locks"));
        assert_eq!(
            reply
                .get_document("asserts")
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["user", "rollovers"]
        );
    });
}

/// `{wiredTiger: 0}` leaves the section out; the process's fields stay.
#[test]
fn a_section_turned_off_is_left_out() {
    with_wt(|c| {
        counted(c);
        let reply = dispatch_full(
            &doc! {"serverStatus": 1, "wiredTiger": 0, "metrics": false, "host": 0},
            c,
        );
        assert!(!reply.contains_key("wiredTiger") && !reply.contains_key("metrics"));
        assert!(reply.contains_key("host") && reply.contains_key("asserts"));
    });
}

#[test]
fn wired_tiger_reports_the_engines_own_statistics() {
    with_wt(|c| {
        let wt = status(c).get_document("wiredTiger").unwrap().clone();
        assert_eq!(wt.get_str("uri").unwrap(), "statistics:");
        assert!(wt.get_str("version").unwrap().starts_with("WiredTiger "));
        // The cache's configured size is the store's: 4G by default.
        assert_eq!(num(&wt, "cache.maximum bytes configured"), 4 << 30);
        let before = num(&wt, "transaction.transaction begins");
        dispatch_full(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let after = status(c);
        assert!(num(&after, "wiredTiger.transaction.transaction begins") > before);
    });
}

#[test]
fn documents_count_as_mongod_counts_them() {
    with_wt(|c| {
        counted(c);
        let paths = [
            "metrics.document.inserted",
            "metrics.document.updated",
            "metrics.document.deleted",
            "metrics.document.returned",
        ];
        let docs: Vec<Bson> = (0..5)
            .map(|i| Bson::Document(doc! {"_id": i, "a": i}))
            .collect();
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&doc! {"insert": "c", "documents": docs}, c);
            }),
            [5, 0, 0, 0]
        );
        // A find in two batches returns every document once.
        assert_eq!(
            moved(c, &paths, |c| {
                let first = dispatch_full(&doc! {"find": "c", "batchSize": 2}, c);
                let id = first.get_document("cursor").unwrap().get_i64("id").unwrap();
                dispatch_full(&doc! {"getMore": id, "collection": "c"}, c);
            }),
            [0, 0, 0, 5]
        );
        // An update counts what it matched, modified or not; an upsert's
        // insert counts nowhere.
        assert_eq!(
            moved(c, &paths, |c| {
                let set = doc! {"q": {"_id": 0}, "u": {"$set": {"a": 0}}};
                let up = doc! {"q": {"_id": 50}, "u": {"$set": {"a": 1}}, "upsert": true};
                dispatch_full(&doc! {"update": "c", "updates": [set, up]}, c);
            }),
            [0, 1, 0, 0]
        );
        // findAndModify: what it changed, and the document it returned.
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(
                    &doc! {"findAndModify": "c", "query": {"_id": 1}, "remove": true},
                    c,
                );
            }),
            [0, 0, 1, 1]
        );
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&doc! {"count": "c"}, c);
                dispatch_full(&doc! {"distinct": "c", "key": "a"}, c);
            }),
            [0, 0, 0, 1 + 5]
        );
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&doc! {"delete": "c", "deletes": [{"q": {}, "limit": 0}]}, c);
            }),
            [0, 0, 5, 0]
        );
    });
}

#[test]
fn commands_errors_and_latencies_count_as_mongods() {
    with_wt(|c| {
        counted(c);
        dispatch_full(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let paths = [
            "metrics.commands.insert.total",
            "metrics.commands.insert.failed",
            "asserts.user",
            "opLatencies.writes.ops",
            "opLatencies.commands.ops",
        ];
        // A duplicate key is a write error: one user assertion, no failure.
        // (The `commands` op is the first `serverStatus`.)
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
            }),
            [1, 0, 1, 1, 1]
        );
        // A command mongod does not know is no assertion.
        let unknown = ["metrics.commands.<UNKNOWN>", "asserts.user"];
        assert_eq!(
            moved(c, &unknown, |c| {
                dispatch_full(&doc! {"noSuchCommand": 1}, c);
            }),
            [1, 0]
        );
        // A command that does not parse is no run of it, and is timed as a
        // command.
        let get_more = [
            "metrics.commands.getMore.total",
            "asserts.user",
            "opLatencies.reads.ops",
            "opLatencies.commands.ops",
        ];
        assert_eq!(
            moved(c, &get_more, |c| {
                dispatch_full(&doc! {"getMore": 5_i32, "collection": "c"}, c);
            }),
            [0, 1, 0, 2]
        );
        // A failed command fails its count.
        let drop_index = [
            "metrics.commands.dropIndexes.total",
            "metrics.commands.dropIndexes.failed",
        ];
        assert_eq!(
            moved(c, &drop_index, |c| {
                dispatch_full(&doc! {"dropIndexes": "c", "index": "zz_1"}, c);
            }),
            [1, 1]
        );
        // `update` counts its pipeline and arrayFilters statements; `create`
        // its validator and `$jsonSchema`.
        let shapes = [
            "metrics.commands.update.pipeline",
            "metrics.commands.update.arrayFilters",
            "metrics.commands.create.validator.total",
            "metrics.commands.create.validator.jsonSchema",
        ];
        assert_eq!(
            moved(c, &shapes, |c| {
                let pipeline = doc! {"q": {}, "u": [{"$set": {"b": 1}}]};
                let filtered =
                    doc! {"q": {}, "u": {"$set": {"l.$[x]": 1}}, "arrayFilters": [{"x": 0}]};
                dispatch_full(&doc! {"update": "c", "updates": [pipeline, filtered]}, c);
                dispatch_full(
                    &doc! {"create": "v", "validator": {"$jsonSchema": {"required": ["x"]}}},
                    c,
                );
            }),
            [1, 1, 1, 1]
        );
    });
}

#[test]
fn multi_document_statements_count_in_metrics_query() {
    with_wt(|c| {
        counted(c);
        let docs: Vec<Bson> = (0..4).map(|i| Bson::Document(doc! {"_id": i})).collect();
        dispatch_full(&doc! {"insert": "c", "documents": docs}, c);
        let paths = [
            "metrics.query.updateManyCount",
            "metrics.query.deleteManyCount",
            "metrics.query.updateDeleteManyDocumentsTotalCount",
            "metrics.query.updateDeleteManyDocumentsMaxCount",
        ];
        // Two multi statements, over 2 and 3 documents; the single one does
        // not count.
        assert_eq!(
            moved(c, &paths, |c| {
                let a = doc! {"q": {"_id": {"$lt": 2}}, "u": {"$set": {"m": 1}}, "multi": true};
                let b = doc! {"q": {"_id": {"$gte": 1}}, "u": {"$set": {"m": 2}}, "multi": true};
                let one = doc! {"q": {"_id": 0}, "u": {"$set": {"m": 3}}};
                dispatch_full(&doc! {"update": "c", "updates": [a, b, one]}, c);
            }),
            [2, 0, 5, 3]
        );
        // An ordered batch stopped before a multi statement does not run it.
        assert_eq!(
            moved(c, &paths, |c| {
                let bad = doc! {"q": {"_id": 0}, "u": {"$bad": {"m": 1}}};
                let all = doc! {"q": {}, "u": {"$set": {"m": 4}}, "multi": true};
                dispatch_full(&doc! {"update": "c", "updates": [bad, all]}, c);
            }),
            [0, 0, 0, 0]
        );
        // Four documents: the largest statement so far (3) moves to 4.
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&doc! {"delete": "c", "deletes": [{"q": {}, "limit": 0}]}, c);
            }),
            [0, 1, 4, 1]
        );
    });
}

fn lsid(n: u8) -> Document {
    doc! {"id": bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![n; 16] }}
}

#[test]
fn transactions_count_as_they_start_and_end() {
    with_wt(|c| {
        counted(c);
        with_transactions(c);
        let paths = [
            "transactions.totalStarted",
            "transactions.totalCommitted",
            "transactions.totalAborted",
            "transactions.currentOpen",
            "transactions.currentInactive",
            "opLatencies.transactions.ops",
        ];
        let statement = |n: i64, start: bool| {
            let mut cmd = doc! {"insert": "c", "documents": [{"x": n}], "lsid": lsid(1),
            "txnNumber": n, "autocommit": false};
            if start {
                cmd.insert("startTransaction", true);
            }
            cmd
        };
        // Open and idle between its statements.
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&statement(1, true), c);
            }),
            [1, 0, 0, 1, 1, 0]
        );
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&statement(1, false), c);
                dispatch_admin(
                    &doc! {"commitTransaction": 1, "lsid": lsid(1), "txnNumber": 1_i64,
                    "autocommit": false},
                    c,
                );
            }),
            [0, 1, 0, -1, -1, 1]
        );
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&statement(2, true), c);
                dispatch_admin(
                    &doc! {"abortTransaction": 1, "lsid": lsid(1), "txnNumber": 2_i64,
                    "autocommit": false},
                    c,
                );
            }),
            [1, 0, 1, 0, 0, 1]
        );
        assert_eq!(
            num(&status(c), "logicalSessionRecordCache.sessionCatalogSize"),
            1
        );
    });
}

#[test]
fn a_retried_write_counts_its_statements() {
    with_wt(|c| {
        counted(c);
        with_transactions(c);
        let paths = [
            "transactions.retriedCommandsCount",
            "transactions.retriedStatementsCount",
        ];
        let insert = doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}],
        "lsid": lsid(2), "txnNumber": 1_i64};
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&insert, c);
            }),
            [0, 0]
        );
        assert_eq!(
            moved(c, &paths, |c| {
                dispatch_full(&insert, c);
            }),
            [1, 2]
        );
    });
}

/// A store opened without statistics (`statistics=(none)`) degrades
/// `serverStatus.wiredTiger` to mongod's error triple; the command still
/// answers `ok: 1` (measured 8.2.11, started with
/// `--wiredTigerEngineConfigString "statistics=(none)"`). This server failed
/// the whole command.
#[test]
fn server_status_without_engine_statistics() {
    use secantus_commands::{CursorRegistry, Storage as CmdStorage};
    use secantus_storage::{Storage as WtStorage, StorageOptions};
    use secantus_storage_adapter::StorageAdapter;

    let dir = std::env::temp_dir().join(format!("secantus-nostats-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wt = WtStorage::open_with_options(
        dir.to_str().unwrap(),
        &StorageOptions {
            wt_config: Some(
                "create,session_max=100,cache_size=64M,log=(enabled=true),statistics=(none)"
                    .to_string(),
            ),
            ..StorageOptions::default()
        },
    )
    .unwrap();
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(Arc::new(wt)));
    let mut c = CommandContext::new(1)
        .with_storage(adapter)
        .with_cursors(Arc::new(CursorRegistry::new()));
    c.db_name = "admin".into();
    let reply = dispatch_full(&doc! {"serverStatus": 1}, &mut c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    let section = reply.get_document("wiredTiger").unwrap();
    assert_eq!(
        section.get_str("error"),
        Ok("unable to retrieve statistics")
    );
    assert_eq!(section.get_i32("code"), Ok(43));
    assert!(section
        .get_str("reason")
        .unwrap()
        .starts_with("unable to open cursor at URI statistics:. reason: "));
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}
