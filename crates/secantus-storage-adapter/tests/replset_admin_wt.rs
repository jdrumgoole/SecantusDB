//! The replica-set, lock, cluster-parameter, plan-cache and parameter
//! commands of a one-member set, over real WiredTiger, and the `local`
//! database mongod does not replicate. Every expectation is what mongod
//! 8.2.11 answered as a replica-set member (2026-10-15).

mod common;

use bson::{doc, Binary, Bson, Document};
use common::{dispatch_full, with_transactions, with_wt};
use secantus_commands::CommandContext;

fn as_set_member(c: &mut CommandContext) {
    c.replica_set_name = Some("rs0".into());
    c.server_address = Some(("127.0.0.1".into(), 27999));
}

/// Run `cmd` on `db`, without the cluster time a set member stamps on
/// every reply.
fn on(db: &str, cmd: &Document, c: &mut CommandContext) -> Document {
    let previous = std::mem::replace(&mut c.db_name, db.into());
    let mut reply = dispatch_full(cmd, c);
    c.db_name = previous;
    reply.remove("$clusterTime");
    reply.remove("operationTime");
    reply
}

fn error(reply: &Document) -> (i32, String) {
    (
        reply.get_i32("code").unwrap_or_else(|_| panic!("{reply}")),
        reply.get_str("errmsg").unwrap_or_default().to_string(),
    )
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

fn oplog_entries_for(ns: &str, c: &mut CommandContext) -> usize {
    batch(&on(
        "local",
        &doc! {"find": "oplog.rs", "filter": {"ns": ns}},
        c,
    ))
    .len()
}

/// A write to `local` writes no oplog entry, so no change stream sees it.
/// This server logged every one.
#[test]
fn a_write_to_local_is_not_logged() {
    with_wt(|c| {
        as_set_member(c);
        on(
            "local",
            &doc! {"insert": "zz", "documents": [{"_id": 1}]},
            c,
        );
        on("t", &doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        assert_eq!(oplog_entries_for("local.zz", c), 0);
        assert_eq!(oplog_entries_for("t.c", c), 1);
    });
}

/// A retryable write to `local` fails each statement that would write, with
/// 20; one that would not runs as usual.
#[test]
fn a_retryable_write_to_local_is_refused() {
    with_wt(|c| {
        as_set_member(c);
        with_transactions(c);
        on(
            "local",
            &doc! {"insert": "zz", "documents": [{"_id": 1}]},
            c,
        );
        let lsid =
            doc! {"id": Binary {subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![7; 16]}};
        let refused = "retryable writes is not supported for unreplicated ns: local.zz";
        let mut txn = 0_i64;
        let mut retryable = |cmd: Document, c: &mut CommandContext| {
            txn += 1;
            let mut cmd = cmd;
            cmd.insert("lsid", lsid.clone());
            cmd.insert("txnNumber", txn);
            on("local", &cmd, c)
        };
        let errors = |reply: &Document| -> Vec<(i32, i32, String)> {
            reply
                .get_array("writeErrors")
                .map(|e| {
                    e.iter()
                        .filter_map(Bson::as_document)
                        .map(|e| {
                            (
                                e.get_i32("index").unwrap(),
                                e.get_i32("code").unwrap(),
                                e.get_str("errmsg").unwrap().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let reply = retryable(
            doc! {"insert": "zz", "documents": [{"_id": 2}, {"_id": 3}], "ordered": false},
            c,
        );
        assert_eq!(
            errors(&reply),
            vec![(0, 20, refused.to_string()), (1, 20, refused.to_string())]
        );
        assert_eq!(reply.get_i32("n"), Ok(0));
        let reply = retryable(
            doc! {"insert": "zz", "documents": [{"_id": 2}, {"_id": 3}]},
            c,
        );
        assert_eq!(errors(&reply).len(), 1);

        let update = format!("Plan executor error during update :: caused by :: {refused}");
        let reply = retryable(
            doc! {"update": "zz", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]},
            c,
        );
        assert_eq!(errors(&reply), vec![(0, 20, update.clone())]);
        assert_eq!(reply.get_i32("nModified"), Ok(0));
        // Nothing matches and nothing is upserted: no write, no error.
        let reply = retryable(
            doc! {"update": "zz", "updates": [{"q": {"_id": 7}, "u": {"$set": {"a": 1}}}]},
            c,
        );
        assert_eq!((errors(&reply), reply.get_i32("n")), (vec![], Ok(0)));
        let reply = retryable(
            doc! {"update": "zz", "updates": [{"q": {"_id": 7}, "u": {"$set": {"a": 1}}, "upsert": true}]},
            c,
        );
        assert_eq!(errors(&reply), vec![(0, 20, update)]);

        let delete = format!("Plan executor error during delete :: caused by :: {refused}");
        let reply = retryable(
            doc! {"delete": "zz", "deletes": [{"q": {"_id": 1}, "limit": 1}]},
            c,
        );
        assert_eq!(errors(&reply), vec![(0, 20, delete)]);

        let reply = retryable(
            doc! {"findAndModify": "zz", "query": {"_id": 1}, "update": {"$set": {"a": 2}}},
            c,
        );
        assert_eq!(
            error(&reply),
            (
                20,
                format!("Plan executor error during findAndModify :: caused by :: {refused}")
            )
        );
        // Only the first document is there: nothing was written.
        let docs = batch(&on("local", &doc! {"find": "zz"}, c));
        assert_eq!(docs, vec![doc! {"_id": 1}]);
    });
}

#[test]
fn replica_set_commands_of_a_one_member_set() {
    with_wt(|c| {
        as_set_member(c);
        assert_eq!(
            error(&on("admin", &doc! {"replSetInitiate": 1}, c)),
            (23, "already initialized".into())
        );
        let note = on("admin", &doc! {"appendOplogNote": 1, "data": {"x": 1}}, c);
        assert_eq!(note.get_f64("ok"), Ok(1.0), "{note}");
        let entries = batch(&on(
            "local",
            &doc! {"find": "oplog.rs", "filter": {"op": "n", "o.x": 1}},
            c,
        ));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].keys().take(3).collect::<Vec<_>>(),
            ["op", "ns", "o"]
        );
        for (cmd, want) in [
            (
                doc! {"appendOplogNote": 1},
                (4, "Missing expected field \"data\"".to_string()),
            ),
            (
                doc! {"appendOplogNote": 1, "data": 5},
                (
                    14,
                    "\"data\" had the wrong type. Expected object, found int".to_string(),
                ),
            ),
            (
                doc! {"replSetResizeOplog": 1, "size": 989.5},
                (
                    2,
                    "BSON field 'size' value must be >= 990, actual value '989.5'".to_string(),
                ),
            ),
            (
                doc! {"replSetResizeOplog": 1, "size": 1_073_741_825.0},
                (
                    2,
                    "BSON field 'size' value must be <= 1.07374e+09, actual value '1.07374e+09'"
                        .to_string(),
                ),
            ),
            (
                doc! {"replSetResizeOplog": 1, "size": f64::NAN},
                (
                    2,
                    "BSON field 'size' value must be >= 990, actual value '0'".to_string(),
                ),
            ),
            (
                doc! {"replSetResizeOplog": 1, "minRetentionHours": -1},
                (
                    2,
                    "BSON field 'minRetentionHours' value must be >= 0, actual value '-1'"
                        .to_string(),
                ),
            ),
            (
                doc! {"replSetResizeOplog": 1, "bogus": 1},
                (
                    40415,
                    "BSON field 'replSetResizeOplog.bogus' is an unknown field.".to_string(),
                ),
            ),
        ] {
            assert_eq!(error(&on("admin", &cmd, c)), want, "{cmd}");
        }
        // The cap is what `listCollections` reports as the oplog's size.
        let resized = on("admin", &doc! {"replSetResizeOplog": 1, "size": 990.0}, c);
        assert_eq!(resized.get_f64("ok"), Ok(1.0), "{resized}");
        let listed = batch(&on(
            "local",
            &doc! {"listCollections": 1, "filter": {"name": "oplog.rs"}},
            c,
        ));
        let options = listed[0].get_document("options").unwrap();
        assert_eq!(
            options
                .get("size")
                .and_then(Bson::as_i64)
                .or_else(|| options.get_i32("size").ok().map(i64::from)),
            Some(990 * 1024 * 1024)
        );
        assert_eq!(options.get_bool("autoIndexId"), Ok(false));
        assert!(listed[0].get("idIndex").is_none());
        // An oplog has no index at all.
        assert_eq!(
            batch(&on("local", &doc! {"listIndexes": "oplog.rs"}, c)),
            vec![]
        );
        let status = on("admin", &doc! {"replSetGetStatus": 1}, c);
        let metrics = status.get_document("electionCandidateMetrics").unwrap();
        assert_eq!(metrics.get_str("lastElectionReason"), Ok("electionTimeout"));
        assert_eq!(metrics.get_i64("electionTerm"), Ok(1));
    });
}

/// A server run without a set name answers as a standalone mongod does.
#[test]
fn replica_set_commands_of_a_standalone() {
    with_wt(|c| {
        for (cmd, code) in [
            (doc! {"replSetInitiate": 1}, 76),
            (doc! {"appendOplogNote": 1, "data": {}}, 76),
            (doc! {"replSetResizeOplog": 1, "size": 990.0}, 26),
        ] {
            assert_eq!(error(&on("admin", &cmd, c)).0, code, "{cmd}");
        }
    });
}

#[test]
fn the_commands_a_tool_sends() {
    with_wt(|c| {
        as_set_member(c);
        assert_eq!(
            on("admin", &doc! {"lockInfo": 1}, c),
            doc! {"lockInfo": [], "ok": 1.0}
        );
        assert_eq!(error(&on("x", &doc! {"lockInfo": 1}, c)).0, 13);
        let features = on("x", &doc! {"features": 1}, c);
        assert!(features.get_i64("oidMachine").is_ok(), "{features}");
        assert!(features.get("js").is_none());

        let params = on(
            "admin",
            &doc! {"getClusterParameter": ["defaultMaxTimeMS", "changeStreamOptions"]},
            c,
        );
        let listed = params.get_array("clusterParameters").unwrap();
        assert_eq!(
            listed[0].as_document().unwrap(),
            &doc! {
                "_id": "defaultMaxTimeMS",
                "clusterParameterTime": Bson::Timestamp(bson::Timestamp {time: 0, increment: 0}),
                "readOperations": 0_i64,
            }
        );
        for (db, cmd, code) in [
            ("admin", doc! {"getClusterParameter": "nope"}, 4),
            ("admin", doc! {"getClusterParameter": []}, 2),
            ("x", doc! {"getClusterParameter": 5}, 14),
            ("x", doc! {"getClusterParameter": "*"}, 13),
        ] {
            assert_eq!(error(&on(db, &cmd, c)).0, code, "{cmd}");
        }

        on("t", &doc! {"insert": "c", "documents": [{"a": 1}]}, c);
        on("t", &doc! {"create": "v", "viewOn": "c", "pipeline": []}, c);
        assert_eq!(
            on("t", &doc! {"planCacheListFilters": "c"}, c),
            doc! {"filters": [], "ok": 1.0}
        );
        assert_eq!(
            on("t", &doc! {"planCacheClear": "nope"}, c),
            doc! {"ok": 1.0}
        );
        assert_eq!(error(&on("t", &doc! {"planCacheClear": "v"}, c)).0, 166);
        assert_eq!(error(&on("t", &doc! {"planCacheClear": 5}, c)).0, 73);
    });
}

/// `cursorTimeoutMillis` and `transactionLifetimeLimitSeconds` change the
/// limits the registries hold a cursor and a transaction to.
#[test]
fn the_registry_limits_are_settable() {
    with_wt(|c| {
        with_transactions(c);
        c.server_params = Some(std::sync::Arc::new(
            secantus_commands::params::ServerParams::new(),
        ));
        on(
            "t",
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}]},
            c,
        );
        let opened = on("t", &doc! {"find": "c", "batchSize": 1}, c);
        let id = opened
            .get_document("cursor")
            .unwrap()
            .get_i64("id")
            .unwrap();
        let set = on(
            "admin",
            &doc! {"setParameter": 1, "cursorTimeoutMillis": 0},
            c,
        );
        assert_eq!(set.get_i64("was"), Ok(600_000), "{set}");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let more = on("t", &doc! {"getMore": id, "collection": "c"}, c);
        assert_eq!(error(&more).0, 43, "{more}");
        let read = on(
            "admin",
            &doc! {"getParameter": 1, "cursorTimeoutMillis": 1},
            c,
        );
        assert_eq!(read.get_i64("cursorTimeoutMillis"), Ok(0));

        let set = on(
            "admin",
            &doc! {"setParameter": 1, "transactionLifetimeLimitSeconds": 30},
            c,
        );
        assert_eq!(set.get_i32("was"), Ok(60), "{set}");
        assert_eq!(c.transactions.as_ref().unwrap().lifetime_seconds(), 30.0);
        for (value, message) in [
            (
                Bson::Int32(0),
                "Invalid value for parameter transactionLifetimeLimitSeconds: 0 is not greater than or equal to 1",
            ),
            (
                Bson::String("x".into()),
                "Failed validating transactionLifetimeLimitSeconds: Unable to coerce value to integral type",
            ),
            (
                Bson::Double(3e9),
                "Failed validating transactionLifetimeLimitSeconds: Out of bounds coercing to integral value",
            ),
            (
                Bson::Double(f64::NAN),
                "Failed validating transactionLifetimeLimitSeconds: Unable to coerce NaN/Inf to integral type",
            ),
        ] {
            let cmd = doc! {"setParameter": 1, "transactionLifetimeLimitSeconds": value};
            assert_eq!(error(&on("admin", &cmd, c)), (2, message.to_string()));
        }
    });
}

/// The set's configuration is kept in `local.system.replset`, so its id
/// is the same each time the server binds, and `replSetGetConfig` reads it.
#[test]
fn the_configuration_is_stored() {
    with_wt(|c| {
        as_set_member(c);
        let storage = c.storage.clone().unwrap();
        secantus_commands::serverinfo::record_replset_config(
            storage.as_ref(),
            "rs0",
            "127.0.0.1",
            1,
        )
        .unwrap();
        secantus_commands::serverinfo::record_replset_config(
            storage.as_ref(),
            "rs0",
            "127.0.0.1",
            27999,
        )
        .unwrap();
        let stored = batch(&on("local", &doc! {"find": "system.replset"}, c));
        assert_eq!(stored.len(), 1);
        let config = on("admin", &doc! {"replSetGetConfig": 1}, c);
        let config = config.get_document("config").unwrap();
        assert_eq!(config, &stored[0]);
        let host = config.get_array("members").unwrap()[0]
            .as_document()
            .unwrap()
            .get_str("host")
            .unwrap()
            .to_string();
        assert_eq!(host, "127.0.0.1:27999");
        let names = batch(&on(
            "local",
            &doc! {"listCollections": 1, "nameOnly": true},
            c,
        ));
        let names: Vec<_> = names
            .iter()
            .filter_map(|d| d.get_str("name").ok())
            .collect();
        assert_eq!(names, ["oplog.rs", "system.replset"]);
    });
}

#[test]
fn validate_reads_system_views() {
    with_wt(|c| {
        on("t", &doc! {"create": "src"}, c);
        on(
            "t",
            &doc! {"create": "v", "viewOn": "src", "pipeline": []},
            c,
        );
        let reply = on("t", &doc! {"validate": "system.views"}, c);
        assert_eq!(reply.get_str("ns"), Ok("t.system.views"), "{reply}");
        assert_eq!(reply.get_i32("nrecords"), Ok(1));
        assert_eq!(reply.get_i32("nIndexes"), Ok(1));
        assert_eq!(reply.get_bool("valid"), Ok(true));
        on("x", &doc! {"create": "c"}, c);
        assert_eq!(error(&on("x", &doc! {"validate": "system.views"}, c)).0, 26);
    });
}

/// A view's definition is in `dbStats`'s `dataSize`, as a `system.views`
/// document is on mongod.
#[test]
fn db_stats_counts_a_views_definition() {
    with_wt(|c| {
        let documents = vec![
            doc! {"_id": 1, "a": "x".repeat(100)},
            doc! {"_id": 2, "a": 1},
        ];
        on("t", &doc! {"insert": "c", "documents": documents}, c);
        let before = on("t", &doc! {"dbStats": 1}, c);
        let stage = doc! {"$match": {"a": {"$gt": 0}}};
        let pipeline = vec![
            stage.clone(),
            stage.clone(),
            stage.clone(),
            stage.clone(),
            stage,
        ];
        on(
            "t",
            &doc! {"create": "v", "viewOn": "c", "pipeline": pipeline},
            c,
        );
        let after = on("t", &doc! {"dbStats": 1}, c);
        let size = |r: &Document| r.get("dataSize").and_then(Bson::as_f64).unwrap();
        // `{_id: "t.v", viewOn: "c", pipeline: [...]}`: 237 bytes (238 for
        // the probe's database `ds`, one letter longer).
        assert_eq!(size(&after) - size(&before), 237.0);
    });
}

#[test]
fn host_info_reads_the_machine() {
    with_wt(|c| {
        let reply = on("admin", &doc! {"hostInfo": 1}, c);
        let system = reply.get_document("system").unwrap();
        for field in [
            "currentTime",
            "hostname",
            "cpuAddrSize",
            "numCores",
            "cpuArch",
            "numaEnabled",
        ] {
            assert!(system.contains_key(field), "{field} in {system}");
        }
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(system.get_i64("memSizeMB").unwrap() > 0, "{system}");
            assert!(system.get_i32("numPhysicalCores").unwrap() > 0, "{system}");
        }
    });
}
