//! The database profiler over real WiredTiger: the `profile` command's
//! settings, which operations are written to `system.profile` and in what
//! shape, and what `system.profile` is. Every expectation is what mongod
//! 8.2.11 answered (2026-10-15), as a one-member replica set.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_transactions, with_wt};
use secantus_commands::CommandContext;

const NOTE: &str = "When a filter expression is set, slowms and sampleRate are not used for \
                    profiling and slow-query log lines.";

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

/// `db`'s profile entries, oldest first.
fn entries(db: &str, c: &mut CommandContext) -> Vec<Document> {
    batch(&on(
        db,
        &doc! {"find": "system.profile", "batchSize": 10_000},
        c,
    ))
}

/// The entries' `op`s and the first key of their `command`.
fn ops(entries: &[Document]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|e| {
            let command = e.get_document("command").unwrap();
            (
                e.get_str("op").unwrap().to_string(),
                command.keys().next().unwrap().to_string(),
            )
        })
        .collect()
}

/// An entry's field names, in order.
fn keys(entry: &Document) -> Vec<&str> {
    entry.keys().map(String::as_str).collect()
}

/// Turn profiling off and read what was profiled. (Reading `system.profile`
/// at level 2 would profile the read.)
fn stop(db: &str, c: &mut CommandContext) -> Vec<Document> {
    on(db, &doc! {"profile": 0}, c);
    entries(db, c)
}

#[test]
fn level_one_or_two_creates_a_capped_system_profile_at_once() {
    with_wt(|c| {
        for (db, level) in [("p1", 1), ("p2", 2)] {
            assert_eq!(
                on(db, &doc! {"profile": level, "slowms": 0}, c),
                doc! {"was": 0, "slowms": 100, "sampleRate": 1.0, "ok": 1.0}
            );
            let listed = batch(&on(
                db,
                &doc! {"listCollections": 1, "filter": {"name": "system.profile"}},
                c,
            ));
            assert_eq!(listed.len(), 1);
            // No `idIndex`: mongod gives system.profile no `_id` index.
            assert_eq!(
                keys(&listed[0]),
                vec!["name", "type", "options", "info"],
                "{listed:?}"
            );
            assert_eq!(
                listed[0].get_document("options").unwrap(),
                &doc! {"capped": true, "size": 1_048_576_i32}
            );
            on(db, &doc! {"profile": 0, "slowms": 100}, c);
            let stats = on(db, &doc! {"collStats": "system.profile"}, c);
            assert_eq!(
                (stats.get_bool("capped"), stats.get_i32("nindexes")),
                (Ok(true), Ok(0))
            );
        }
        // Level 0 creates nothing, filter or no filter.
        on("p0", &doc! {"profile": 0, "filter": {"op": "query"}}, c);
        assert_eq!(
            batch(&on("p0", &doc! {"listCollections": 1}, c)),
            Vec::<Document>::new()
        );
    });
}

#[test]
fn a_filter_is_stored_as_mongod_parses_it_and_reported() {
    with_wt(|c| {
        // Setting one reports the settings it replaces, with the note.
        assert_eq!(
            on("f", &doc! {"profile": 0, "filter": {"op": "query"}}, c),
            doc! {"was": 0, "slowms": 100, "sampleRate": 1.0, "note": NOTE, "ok": 1.0}
        );
        assert_eq!(
            on("f", &doc! {"profile": -1}, c),
            doc! {
                "was": 0, "slowms": 100, "sampleRate": 1.0,
                "filter": {"op": {"$eq": "query"}}, "note": NOTE, "ok": 1.0,
            }
        );
        // A level outside 0..=2 leaves the level alone and still takes the
        // filter and slowms.
        on(
            "f",
            &doc! {"profile": 5, "filter": {"millis": {"$gt": 5}, "ns": "a.b"}, "slowms": 7},
            c,
        );
        let read = on("f", &doc! {"profile": -1}, c);
        assert_eq!(
            (
                read.get_i32("was"),
                read.get_i32("slowms"),
                read.get_document("filter").unwrap()
            ),
            (
                Ok(0),
                Ok(7),
                &doc! {"$and": [{"millis": {"$gt": 5}}, {"ns": {"$eq": "a.b"}}]}
            )
        );
        // "unset" removes it; the reply still carries the old one.
        let unset = on(
            "f",
            &doc! {"profile": 0, "filter": "unset", "slowms": 100},
            c,
        );
        assert!(unset.contains_key("filter") && unset.contains_key("note"));
        assert_eq!(
            on("f", &doc! {"profile": -1}, c),
            doc! {"was": 0, "slowms": 100, "sampleRate": 1.0, "ok": 1.0}
        );
        // Null is not "unset".
        assert_eq!(
            error(&on("f", &doc! {"profile": 0, "filter": Bson::Null}, c)),
            (2, "Expected an object, or the string 'unset'.".into())
        );
        // Only a profile entry's fields may be named.
        assert_eq!(
            error(&on(
                "f",
                &doc! {"profile": 0, "filter": {"zz": 1, "op": 1}},
                c
            )),
            (
                4910200,
                "No such field (or fields) available for profile filter: zz".into()
            )
        );
        assert_eq!(
            error(&on("f", &doc! {"profile": 0, "filter": {"$where": "1"}}, c)),
            (2, "$where is not allowed in this context".into())
        );
        // Settings are per database.
        on("g", &doc! {"profile": 1, "filter": {"op": "insert"}}, c);
        assert_eq!(on("f", &doc! {"profile": -1}, c).get("filter"), None);
        on("g", &doc! {"profile": 0, "filter": "unset"}, c);
    });
}

#[test]
fn dropping_the_database_forgets_its_settings() {
    with_wt(|c| {
        on("d", &doc! {"profile": 2, "filter": {"op": "query"}}, c);
        on("d", &doc! {"insert": "t", "documents": [{"_id": 1}]}, c);
        assert_eq!(on("d", &doc! {"dropDatabase": 1}, c).get_f64("ok"), Ok(1.0));
        let read = on("d", &doc! {"profile": -1}, c);
        assert_eq!((read.get_i32("was"), read.get("filter")), (Ok(0), None));
        assert_eq!(
            batch(&on("d", &doc! {"listCollections": 1}, c)),
            Vec::<Document>::new()
        );
    });
}

#[test]
fn level_two_writes_each_operation_in_mongods_shape() {
    with_wt(|c| {
        on(
            "s",
            &doc! {"insert": "t", "documents": [{"_id": 0}, {"_id": 1}, {"_id": 2}]},
            c,
        );
        on("s", &doc! {"profile": 2}, c);
        on("s", &doc! {"find": "t", "filter": {"_id": {"$gt": 0}}}, c);
        on(
            "s",
            &doc! {"insert": "t", "documents": [{"_id": 5}, {"_id": 5}], "ordered": false},
            c,
        );
        on("s", &doc! {"find": "t", "filter": {"$bad": 1}}, c);
        on("s", &doc! {"count": "t"}, c);
        on("s", &doc! {"distinct": "t", "key": "_id"}, c);
        on(
            "s",
            &doc! {"findAndModify": "t", "query": {"_id": 1}, "update": {"$set": {"a": 1}}},
            c,
        );
        let profiled = stop("s", c);
        assert_eq!(
            ops(&profiled),
            vec![
                ("query".into(), "find".into()),
                ("insert".into(), "insert".into()),
                ("query".into(), "find".into()),
                ("command".into(), "count".into()),
                ("command".into(), "distinct".into()),
                ("command".into(), "findAndModify".into()),
            ]
        );
        let common = [
            "responseLength",
            "protocol",
            "millis",
            "ts",
            "client",
            "allUsers",
            "user",
        ];
        let with = |head: &[&'static str]| -> Vec<&'static str> {
            head.iter().chain(common.iter()).copied().collect()
        };
        let find = &profiled[0];
        assert_eq!(
            keys(find),
            with(&[
                "op",
                "ns",
                "command",
                "nBatches",
                "cursorExhausted",
                "numYield",
                "nreturned"
            ])
        );
        assert_eq!(
            (
                find.get_str("ns"),
                find.get_i32("nreturned"),
                find.get_str("protocol"),
                find.get_document("command").unwrap(),
            ),
            (
                Ok("s.t"),
                Ok(2),
                Ok("op_msg"),
                &doc! {"find": "t", "filter": {"_id": {"$gt": 0}}},
            )
        );
        assert!(!find.contains_key("_id"), "a profile entry has no _id");
        // A duplicate is a write error, not a failure of the command.
        assert_eq!(profiled[1].get_i32("ninserted"), Ok(1));
        assert!(!profiled[1].contains_key("ok"));
        assert_eq!(
            keys(&profiled[2]),
            with(&["op", "ns", "command", "numYield", "ok", "errMsg", "errName", "errCode"])
        );
        assert_eq!(
            (
                profiled[2].get_f64("ok"),
                profiled[2].get_str("errName"),
                profiled[2].get_i32("errCode")
            ),
            (Ok(0.0), Ok("BadValue"), Ok(2))
        );
        assert_eq!(profiled[3].get_i32("nreturned"), Ok(1));
        assert_eq!(profiled[4].get_i32("nreturned"), Ok(4));
        let fam = &profiled[5];
        assert_eq!(
            (
                fam.get_i32("nMatched"),
                fam.get_i32("nModified"),
                fam.get_i32("nUpserted"),
                fam.get_i32("nreturned"),
            ),
            (Ok(1), Ok(1), Ok(0), Ok(1))
        );
    });
}

#[test]
fn update_and_delete_are_profiled_by_statement() {
    with_wt(|c| {
        on(
            "w",
            &doc! {"insert": "t", "documents": [{"_id": 1, "s": "x"}, {"_id": 2}]},
            c,
        );
        on("w", &doc! {"profile": 2}, c);
        on(
            "w",
            &doc! {"update": "t", "ordered": false, "updates": [
                {"u": {"$set": {"a": 1}}, "q": {"_id": 2}, "hint": {"_id": 1}},
                {"q": {"_id": 1}, "u": {"$inc": {"s": 1}}},
                {"q": {"_id": 1}, "u": {"$bad": 1}},
                {"q": {"_id": 9}, "u": {"$set": {"a": 1}}, "upsert": true},
            ]},
            c,
        );
        on(
            "w",
            &doc! {"delete": "t", "deletes": [{"q": {"_id": 9}, "limit": 1}, {"q": {"_id": 8}, "limit": 0}]},
            c,
        );
        let profiled = stop("w", c);
        assert_eq!(
            ops(&profiled),
            vec![
                ("update".into(), "q".into()),
                ("update".into(), "q".into()),
                ("update".into(), "q".into()),
                ("remove".into(), "q".into()),
                ("remove".into(), "q".into()),
            ],
            "a statement refused before it runs has no entry"
        );
        assert_eq!(
            profiled[0].get_document("command").unwrap(),
            &doc! {"q": {"_id": 2}, "u": {"$set": {"a": 1}}, "hint": {"_id": 1},
            "multi": false, "upsert": false}
        );
        let tail = ["millis", "ts", "client", "allUsers", "user"];
        let mut want = vec![
            "op",
            "ns",
            "command",
            "nMatched",
            "nModified",
            "nUpserted",
            "numYield",
        ];
        want.extend(tail);
        assert_eq!(keys(&profiled[0]), want);
        assert_eq!(
            (
                profiled[1].get_i32("errCode"),
                profiled[1].get_str("errName")
            ),
            (Ok(14), Ok("TypeMismatch"))
        );
        assert_eq!(
            (
                profiled[2].get_i32("nMatched"),
                profiled[2].get_i32("nUpserted")
            ),
            (Ok(0), Ok(1))
        );
        assert_eq!(
            profiled[3].get_document("command").unwrap(),
            &doc! {"q": {"_id": 9}, "limit": 1}
        );
        assert_eq!(
            (
                profiled[3].get_i32("ndeleted"),
                profiled[4].get_i32("ndeleted")
            ),
            (Ok(1), Ok(0))
        );
    });
}

#[test]
fn level_one_takes_slowms_and_sample_rate_or_the_filter_instead() {
    with_wt(|c| {
        on("l", &doc! {"insert": "t", "documents": [{"_id": 1}]}, c);
        // Nothing here takes 100 ms.
        on("l", &doc! {"profile": 1, "slowms": 100}, c);
        on("l", &doc! {"find": "t"}, c);
        assert_eq!(stop("l", c).len(), 0);
        on("l", &doc! {"profile": 1, "slowms": 0, "sampleRate": 0.0}, c);
        on("l", &doc! {"find": "t"}, c);
        assert_eq!(stop("l", c).len(), 0);
        on("l", &doc! {"profile": 1, "slowms": 0, "sampleRate": 1.0}, c);
        on("l", &doc! {"find": "t"}, c);
        assert_eq!(stop("l", c).len(), 1);
        // A filter replaces slowms and sampleRate...
        on(
            "l",
            &doc! {"profile": 1, "slowms": 100_000, "sampleRate": 0.0, "filter": {"op": "insert"}},
            c,
        );
        on("l", &doc! {"find": "t"}, c);
        on("l", &doc! {"insert": "t", "documents": [{"_id": 2}]}, c);
        let profiled = stop("l", c);
        assert_eq!(
            ops(&profiled[1..]),
            vec![("insert".into(), "insert".into())]
        );
        // ...but level 2 profiles everything regardless.
        on("l", &doc! {"profile": 2}, c);
        on("l", &doc! {"find": "t"}, c);
        on("l", &doc! {"profile": 0, "filter": "unset"}, c);
        let profiled = entries("l", c);
        assert_eq!(ops(&profiled[2..]), vec![("query".into(), "find".into())]);
    });
}

#[test]
fn what_mongod_does_not_profile_is_not_profiled() {
    with_wt(|c| {
        on("n", &doc! {"insert": "t", "documents": [{"_id": 1}]}, c);
        on("n", &doc! {"profile": 2}, c);
        on("n", &doc! {"ping": 1}, c);
        on("n", &doc! {"listCollections": 1}, c);
        on("n", &doc! {"validate": "t"}, c);
        on("n", &doc! {"dbHash": 1}, c);
        // Refused while its arguments were read.
        on("n", &doc! {"find": "t", "bogus": 1}, c);
        on(
            "n",
            &doc! {"aggregate": "t", "pipeline": [{"$bogus": 1}], "cursor": {}},
            c,
        );
        on("n", &doc! {"getMore": 12345_i64, "collection": "t"}, c);
        // A drop of nothing, and a create of what is there.
        on("n", &doc! {"drop": "nothing"}, c);
        on("n", &doc! {"create": "t"}, c);
        // But a read of system.profile is profiled.
        on("n", &doc! {"count": "system.profile"}, c);
        assert_eq!(ops(&stop("n", c)), vec![("command".into(), "count".into())]);
    });
}

#[test]
fn system_profile_is_the_profilers_own() {
    with_wt(|c| {
        as_set_member(c);
        on("o", &doc! {"insert": "t", "documents": [{"_id": 1}]}, c);
        on("o", &doc! {"profile": 2}, c);
        on("o", &doc! {"find": "t"}, c);
        // A client still cannot write it.
        assert_eq!(
            error(&on(
                "o",
                &doc! {"insert": "system.profile", "documents": [{"x": 1}]},
                c
            )),
            (73, "cannot write to o.system.profile".into())
        );
        // Nor drop it while profiling.
        assert_eq!(
            error(&on("o", &doc! {"drop": "system.profile"}, c)),
            (
                20,
                "turn off profiling before dropping system.profile collection".into()
            )
        );
        on("o", &doc! {"profile": 0}, c);
        // It has no `_id` index, and its documents no `_id`.
        assert_eq!(
            batch(&on("o", &doc! {"listIndexes": "system.profile"}, c)),
            Vec::<Document>::new()
        );
        assert!(entries("o", c).iter().all(|e| !e.contains_key("_id")));
        // Neither it nor its documents reach the oplog.
        let logged = batch(&on(
            "local",
            &doc! {"find": "oplog.rs", "filter": {"$or": [
                {"ns": "o.system.profile"},
                {"ns": "o.$cmd", "o.create": "system.profile"},
            ]}},
            c,
        ));
        assert_eq!(logged, Vec::<Document>::new());
        assert_eq!(
            on("o", &doc! {"drop": "system.profile"}, c),
            doc! {"nIndexesWas": 0, "ns": "o.system.profile", "ok": 1.0}
        );
        // A profiled operation recreates it.
        on("o", &doc! {"profile": 2}, c);
        on("o", &doc! {"find": "t"}, c);
        assert_eq!(stop("o", c).len(), 1);
    });
}

#[test]
fn it_stays_within_its_one_megabyte() {
    with_wt(|c| {
        on("cap", &doc! {"insert": "t", "documents": [{"_id": 1}]}, c);
        on("cap", &doc! {"profile": 2}, c);
        let payload = "x".repeat(4_000);
        for i in 0..400 {
            on(
                "cap",
                &doc! {"find": "t", "filter": {"pad": &payload, "i": i}},
                c,
            );
        }
        on("cap", &doc! {"profile": 0}, c);
        let stats = on("cap", &doc! {"collStats": "system.profile"}, c);
        let size = stats.get_i32("size").map(i64::from).unwrap();
        let count = stats.get_i32("count").unwrap();
        assert!(size <= 1_048_576, "{stats}");
        assert!(count < 400 && count > 100, "{stats}");
    });
}

#[test]
fn merge_statements_are_profiled_where_they_write_and_out_counts_its_documents() {
    with_wt(|c| {
        on(
            "m",
            &doc! {"insert": "t", "documents": [{"_id": 0}, {"_id": 1}, {"_id": 2}]},
            c,
        );
        on("m", &doc! {"insert": "into", "documents": [{"_id": 1}]}, c);
        on("other", &doc! {"profile": 2}, c);
        // Only the target database is profiled.
        on(
            "m",
            &doc! {"aggregate": "t", "pipeline": [{"$merge": {"into": {"db": "other", "coll": "x"}}}], "cursor": {}},
            c,
        );
        let other = stop("other", c);
        assert_eq!(ops(&other), vec![("update".into(), "q".into()); 3]);
        assert_eq!(other[0].get_str("ns"), Ok("other.x"));
        assert_eq!(
            other[0].get_document("command").unwrap(),
            &doc! {"q": {"_id": 0}, "u": {"$set": {"_id": 0}}, "multi": false, "upsert": true}
        );
        on("m", &doc! {"profile": 2}, c);
        on(
            "m",
            &doc! {"aggregate": "t", "pipeline": [{"$merge": {"into": "into"}}], "cursor": {}},
            c,
        );
        on(
            "m",
            &doc! {"aggregate": "t", "pipeline": [{"$match": {"_id": {"$lt": 2}}}, {"$out": "o"}], "cursor": {}},
            c,
        );
        let profiled = stop("m", c);
        assert_eq!(
            ops(&profiled),
            vec![
                ("update".into(), "q".into()),
                ("update".into(), "q".into()),
                ("update".into(), "q".into()),
                ("command".into(), "aggregate".into()),
                ("insert".into(), "aggregate".into()),
            ]
        );
        assert_eq!(
            (
                profiled[1].get_i32("nMatched"),
                profiled[1].get_i32("nUpserted")
            ),
            (Ok(1), Ok(0))
        );
        assert_eq!(profiled[4].get_i32("ninserted"), Ok(2));
    });
}

#[test]
fn a_transactions_operations_are_profiled_whatever_becomes_of_it() {
    with_wt(|c| {
        as_set_member(c);
        with_transactions(c);
        on("x", &doc! {"insert": "t", "documents": [{"_id": 0}]}, c);
        on("x", &doc! {"profile": 2}, c);
        let lsid = doc! {"id": bson::Binary {subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![3; 16]}};
        on(
            "x",
            &doc! {"insert": "t", "documents": [{"_id": 1}], "lsid": lsid.clone(),
            "txnNumber": 1_i64, "startTransaction": true, "autocommit": false},
            c,
        );
        common::dispatch_admin(
            &doc! {"abortTransaction": 1, "lsid": lsid, "txnNumber": 1_i64, "autocommit": false},
            c,
        );
        let profiled = stop("x", c);
        assert_eq!(ops(&profiled), vec![("insert".into(), "insert".into())]);
        assert_eq!(
            batch(&on("x", &doc! {"find": "t"}, c)),
            vec![doc! {"_id": 0}]
        );
    });
}

#[test]
fn response_length_is_the_reply_on_the_wire() {
    with_wt(|c| {
        on("r", &doc! {"profile": 2}, c);
        c.db_name = "r".into();
        let reply = dispatch_full(&doc! {"count": "nothing", "$db": "r"}, c);
        let profiled = stop("r", c);
        let body = bson::to_vec(&reply).unwrap().len() as i32;
        // 16-byte header, 4 flag bytes and a section kind.
        assert_eq!(profiled[0].get_i32("responseLength"), Ok(body + 21));
    });
}
