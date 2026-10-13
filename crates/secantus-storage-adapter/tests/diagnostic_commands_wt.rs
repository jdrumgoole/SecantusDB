//! The statistics commands over real WiredTiger: `dataSize` over a key range,
//! `$collStats` and `$indexStats`, and the commands that read no collection.
//! Every expectation is what mongod 8.2.11 answered to the same command over
//! the same documents (`tools/probes/diagnostic_commands.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn seed(c: &mut CommandContext) {
    dispatch_full(&doc! { "drop": "c" }, c);
    let documents = vec![
        doc! { "_id": 1, "v": 5 },
        doc! { "_id": 2, "v": "x" },
        doc! { "_id": 3, "v": [1, 2] },
    ];
    let reply = dispatch_full(&doc! { "insert": "c", "documents": documents }, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    let index = doc! { "createIndexes": "c", "indexes": [{ "key": { "v": 1 }, "name": "v_1" }] };
    assert_eq!(dispatch_full(&index, c).get_f64("ok"), Ok(1.0));
}

fn number(reply: &Document, field: &str) -> i64 {
    match reply.get(field) {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        other => panic!("{field}: {other:?} in {reply}"),
    }
}

fn first(reply: &Document) -> Document {
    let batch = reply
        .get_document("cursor")
        .and_then(|c| c.get_array("firstBatch"))
        .unwrap_or_else(|_| panic!("{reply}"));
    batch[0].as_document().expect("a document").clone()
}

fn aggregate(c: &mut CommandContext, coll: Bson, pipeline: Vec<Document>) -> Document {
    dispatch_full(
        &doc! { "aggregate": coll, "pipeline": pipeline, "cursor": {} },
        c,
    )
}

/// `dataSize` with `min` and `max` sizes the documents of that key range,
/// not the collection: this server once answered the whole collection for
/// any range.
#[test]
fn data_size_reads_a_key_range() {
    with_wt(|c| {
        seed(c);
        let ns = format!("{}.c", c.db_name);
        let whole = dispatch_full(&doc! { "dataSize": &ns }, c);
        assert_eq!(
            (number(&whole, "size"), number(&whole, "numObjects")),
            (80, 3)
        );
        assert_eq!(whole.get_bool("estimate"), Ok(false));

        let ranged = doc! { "dataSize": &ns, "min": { "_id": 2 }, "max": { "_id": 3 } };
        let reply = dispatch_full(&ranged, c);
        assert_eq!(
            (number(&reply, "size"), number(&reply, "numObjects")),
            (23, 1)
        );

        // The range is the index's, over every type: a string bound is above
        // every number.
        let mixed = doc! { "dataSize": &ns, "min": { "_id": 2 }, "max": { "_id": "z" } };
        assert_eq!(number(&dispatch_full(&mixed, c), "numObjects"), 2);

        for (request, code) in [
            (doc! { "dataSize": &ns, "min": { "_id": 2 } }, 2),
            (doc! { "dataSize": &ns, "max": { "_id": 2 } }, 2),
            // No index leads with `zz`, and the one on `v` holds an array.
            (
                doc! { "dataSize": &ns, "min": { "zz": 2 }, "max": { "zz": 3 } },
                96,
            ),
            (
                doc! { "dataSize": &ns, "min": { "v": 0 }, "max": { "v": 9 } },
                96,
            ),
            (doc! { "dataSize": &ns, "min": 1, "max": 2 }, 14),
        ] {
            let reply = dispatch_full(&request, c);
            assert_eq!(reply.get_i32("code"), Ok(code), "{request}: {reply}");
        }
    });
}

/// The `storageStats` of a `$collStats` stage are the numbers `collStats`
/// gives. The stage once answered a size of 0 for every collection.
#[test]
fn coll_stats_stage_gives_the_sizes_the_command_gives() {
    with_wt(|c| {
        seed(c);
        let command = dispatch_full(&doc! { "collStats": "c" }, c);
        let stage = first(&aggregate(
            c,
            Bson::String("c".into()),
            vec![doc! { "$collStats": { "storageStats": {}, "count": {} } }],
        ));
        let stats = stage.get_document("storageStats").expect("storageStats");
        for field in ["size", "count", "avgObjSize", "nindexes"] {
            assert_eq!(number(stats, field), number(&command, field), "{field}");
        }
        assert_eq!(
            (number(stats, "size"), number(stats, "avgObjSize")),
            (80, 26)
        );
        assert_eq!(stats.get_bool("capped"), Ok(false));
        assert_eq!(number(&stage, "count"), 3);

        let scaled = first(&aggregate(
            c,
            Bson::String("c".into()),
            vec![doc! { "$collStats": { "storageStats": { "scale": 2 } } }],
        ));
        assert_eq!(
            number(scaled.get_document("storageStats").unwrap(), "size"),
            40
        );
    });
}

#[test]
fn the_statistics_stages_check_what_they_are_given() {
    with_wt(|c| {
        seed(c);
        let coll = || Bson::String("c".into());
        for (pipeline, code) in [
            (vec![doc! { "$collStats": 1 }], 5447000),
            (vec![doc! { "$collStats": { "bogus": 1 } }], 40415),
            (vec![doc! { "$collStats": { "count": 1 } }], 14),
            (
                vec![doc! { "$collStats": { "storageStats": { "scale": 0 } } }],
                2,
            ),
            (
                vec![doc! { "$collStats": { "storageStats": { "scale": "x" } } }],
                14,
            ),
            (
                vec![doc! { "$collStats": { "latencyStats": { "histograms": "x" } } }],
                14,
            ),
            (
                vec![doc! { "$match": {} }, doc! { "$collStats": {} }],
                40602,
            ),
            (vec![doc! { "$indexStats": 1 }], 28803),
            (vec![doc! { "$indexStats": { "bogus": 1 } }], 28803),
            (
                vec![doc! { "$match": {} }, doc! { "$indexStats": {} }],
                40602,
            ),
        ] {
            let reply = aggregate(c, coll(), pipeline.clone());
            assert_eq!(reply.get_i32("code"), Ok(code), "{pipeline:?}: {reply}");
        }
        // A count of a collection that is not there is refused; its
        // statistics without one are not.
        let missing = aggregate(
            c,
            Bson::String("nosuch".into()),
            vec![doc! { "$collStats": { "count": {} } }],
        );
        assert_eq!(missing.get_i32("code"), Ok(26), "{missing}");

        let index = first(&aggregate(c, coll(), vec![doc! { "$indexStats": {} }]));
        let spec = index.get_document("spec").expect("spec");
        assert_eq!(spec.get_i32("v"), Ok(2));
        assert!(matches!(
            index.get_document("accesses").unwrap().get("since"),
            Some(Bson::DateTime(_))
        ));
    });
}

/// `{aggregate: 1}` needs a first stage that makes its own documents, and
/// the stages that read no collection run only where mongod runs them.
#[test]
fn a_collectionless_aggregate_needs_a_stage_that_reads_no_collection() {
    with_wt(|c| {
        let one = || Bson::Int32(1);
        for (pipeline, message) in [
            (vec![], "{aggregate: 1} is not valid for an empty pipeline."),
            (
                vec![doc! { "$match": {} }],
                "{aggregate: 1} is not valid for '$match'; a collection is required.",
            ),
            (
                vec![doc! { "$currentOp": {} }],
                "$currentOp must be run against the 'admin' database with {aggregate: 1}",
            ),
        ] {
            let reply = aggregate(c, one(), pipeline);
            assert_eq!(reply.get_i32("code"), Ok(73), "{reply}");
            assert_eq!(reply.get_str("errmsg"), Ok(message));
        }
        let documents = aggregate(c, one(), vec![doc! { "$documents": [{ "a": 1 }] }]);
        assert_eq!(first(&documents), doc! { "a": 1 });

        let sessions = aggregate(
            c,
            Bson::String("c".into()),
            vec![doc! { "$listSessions": {} }],
        );
        assert_eq!(
            sessions.get_str("errmsg"),
            Ok("$listSessions may only be run against config.system.sessions")
        );
        c.db_name = "admin".to_string();
        for (pipeline, code) in [
            (vec![doc! { "$currentOp": 1 }], 14),
            (vec![doc! { "$currentOp": { "bogus": 1 } }], 40415),
            (vec![doc! { "$currentOp": { "allUsers": "x" } }], 14),
            (vec![doc! { "$listLocalSessions": { "bogus": 1 } }], 40415),
        ] {
            let reply = aggregate(c, one(), pipeline.clone());
            assert_eq!(reply.get_i32("code"), Ok(code), "{pipeline:?}: {reply}");
        }
        // No command has come in a session, so there is none to list.
        let local = aggregate(c, one(), vec![doc! { "$listLocalSessions": {} }]);
        let batch = local
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch");
        assert_eq!(batch.map(Vec::len), Ok(0), "{local}");
    });
}

/// `serverStatus` counts the commands the server has run, as mongod counts
/// them: an insert by its documents, `find` and `aggregate` as queries.
#[test]
fn server_status_counts_what_ran() {
    with_wt(|c| {
        let stats = std::sync::Arc::new(secantus_commands::topstats::TopStats::new());
        c.top_stats = Some(stats);
        seed(c);
        let counters = |c: &mut CommandContext| {
            dispatch_full(&doc! { "serverStatus": 1 }, c)
                .get_document("opcounters")
                .expect("opcounters")
                .clone()
        };
        let before = counters(c);
        dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 10 }, { "_id": 11 }] },
            c,
        );
        dispatch_full(&doc! { "find": "c" }, c);
        let update = doc! { "q": { "_id": 10 }, "u": { "$set": { "v": 1 } } };
        dispatch_full(&doc! { "update": "c", "updates": [update] }, c);
        dispatch_full(
            &doc! { "delete": "c", "deletes": [{ "q": { "_id": 11 }, "limit": 1 }] },
            c,
        );
        aggregate(c, Bson::String("c".into()), vec![]);
        dispatch_full(&doc! { "count": "c" }, c);
        let after = counters(c);
        let moved: Vec<i64> = ["insert", "query", "update", "delete", "getmore", "command"]
            .iter()
            .map(|name| after.get_i64(name).unwrap() - before.get_i64(name).unwrap())
            .collect();
        assert_eq!(moved, [2, 2, 1, 1, 0, 2]);
    });
}

/// `validate` reads the collection: the keys it reports are the keys mongod
/// holds for the documents (4 for `5`, `"x"` and `[1, 2]`, where one per
/// document was reported before), and the verdict is the store's own check.
#[test]
fn validate_checks_the_collection_and_counts_its_keys() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch_full(&doc! { "validate": "c" }, c);
        assert_eq!(reply.get_bool("valid"), Ok(true), "{reply}");
        assert_eq!(number(&reply, "nrecords"), 3);
        let keys = reply.get_document("keysPerIndex").expect("keysPerIndex");
        assert_eq!((number(keys, "_id_"), number(keys, "v_1")), (3, 4));
        assert_eq!(reply.get_array("errors").map(Vec::len), Ok(0));
        let details = reply.get_document("indexDetails").unwrap();
        assert_eq!(
            details.get_document("v_1").unwrap().get_bool("valid"),
            Ok(true)
        );
    });
}
