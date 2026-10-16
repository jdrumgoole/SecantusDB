//! Smaller command-surface differences from mongod, over real WiredTiger:
//! views, cursors, clustered collections, update pipelines and the TTL
//! monitor's parameters. Every expectation is what mongod 8.2.11 answered to
//! the same command; each test names the backlog item it closed.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn code(reply: &Document) -> i32 {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    reply.get_i32("code").unwrap()
}

fn first_batch(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("{reply}"))
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn ids(reply: &Document) -> Vec<i32> {
    first_batch(reply)
        .iter()
        .map(|d| d.get_i32("_id").unwrap())
        .collect()
}

fn views_over_base(c: &mut CommandContext) {
    dispatch_full(
        &doc! { "insert": "base", "documents": [
            { "_id": 0, "a": 3 }, { "_id": 1, "a": 1 }, { "_id": 2, "a": 2 },
        ] },
        c,
    );
    dispatch_full(
        &doc! { "createIndexes": "base", "indexes": [{ "key": { "a": 1 }, "name": "a_1" }] },
        c,
    );
    dispatch_full(
        &doc! { "create": "v", "viewOn": "base", "pipeline": [{ "$match": {} }] },
        c,
    );
    dispatch_full(
        &doc! { "create": "v0", "viewOn": "base", "pipeline": [] },
        c,
    );
}

/// RM-108: a hint on a view names an index of the collection beneath it.
/// One naming none is refused (2) by `find`, `count` and `distinct`; one
/// naming an index sets the order, and `$natural: -1` reverses it.
#[test]
fn a_hint_on_a_view_names_an_index_of_its_base() {
    with_wt(|c| {
        views_over_base(c);
        for cmd in [
            doc! { "find": "v", "hint": "nope_1" },
            doc! { "count": "v", "hint": "nope_1" },
            doc! { "distinct": "v", "key": "a", "hint": "nope_1" },
            doc! { "find": "v0", "hint": { "nope": 1 } },
        ] {
            assert_eq!(code(&dispatch_full(&cmd, c)), 2, "{cmd}");
        }
        let hinted = dispatch_full(&doc! { "find": "v0", "hint": "a_1" }, c);
        assert_eq!(ids(&hinted), [1, 2, 0]);
        let reversed = dispatch_full(&doc! { "find": "v0", "hint": { "$natural": -1 } }, c);
        assert_eq!(ids(&reversed), [2, 1, 0]);
        let counted = dispatch_full(&doc! { "count": "v", "hint": "a_1" }, c);
        assert_eq!(
            counted
                .get_i64("n")
                .ok()
                .or(counted.get_i32("n").ok().map(i64::from)),
            Some(3)
        );
    });
}

/// RM-108: `$indexStats` through a view with a pipeline is no longer the
/// first stage (40602); through one without, it reports the base's indexes.
#[test]
fn index_stats_through_a_view() {
    with_wt(|c| {
        views_over_base(c);
        let refused = dispatch_full(
            &doc! { "aggregate": "v", "pipeline": [{ "$indexStats": {} }], "cursor": {} },
            c,
        );
        assert_eq!(code(&refused), 40602);
        let stats = dispatch_full(
            &doc! { "aggregate": "v0", "pipeline": [{ "$indexStats": {} }], "cursor": {} },
            c,
        );
        let mut names: Vec<String> = first_batch(&stats)
            .iter()
            .map(|d| d.get_str("name").unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["_id_", "a_1"]);
        assert!(matches!(
            stats.get("ok"),
            Some(Bson::Double(_)) | Some(Bson::Int32(_))
        ));
    });
}

/// RM-100: a clustered collection's clustered `_id` key is listed by
/// `listIndexes` but not counted by `createIndexes`, `dropIndexes` or `drop`.
#[test]
fn a_clustered_key_is_not_counted_as_an_index() {
    with_wt(|c| {
        dispatch_full(
            &doc! { "create": "k", "clusteredIndex": { "key": { "_id": 1 }, "unique": true } },
            c,
        );
        let built = dispatch_full(
            &doc! { "createIndexes": "k", "indexes": [{ "key": { "a": 1 }, "name": "a_1" }] },
            c,
        );
        assert_eq!(built.get_i32("numIndexesBefore"), Ok(0), "{built}");
        assert_eq!(built.get_i32("numIndexesAfter"), Ok(1), "{built}");
        dispatch_full(
            &doc! { "createIndexes": "k", "indexes": [{ "key": { "b": 1 }, "name": "b_1" }] },
            c,
        );
        let dropped = dispatch_full(&doc! { "dropIndexes": "k", "index": "b_1" }, c);
        assert_eq!(dropped.get_i32("nIndexesWas"), Ok(2), "{dropped}");
        let gone = dispatch_full(&doc! { "drop": "k" }, c);
        assert_eq!(gone.get_i32("nIndexesWas"), Ok(1), "{gone}");
    });
}

fn write_error(reply: &Document) -> (i32, String) {
    let e = reply
        .get_array("writeErrors")
        .unwrap_or_else(|_| panic!("{reply}"))[0]
        .as_document()
        .unwrap()
        .clone();
    (
        e.get_i32("code").unwrap(),
        e.get_str("errmsg").unwrap().to_string(),
    )
}

/// RM-101: the `_id` a pipeline update leaves is checked for storage before
/// immutability. An upsert's array `_id` is 54 (as a replacement's is); on a
/// stored document an array or regex is 53 and a `$` field inside it 52.
#[test]
fn a_pipeline_update_checks_the_id_it_leaves() {
    with_wt(|c| {
        let cases: [(&str, Document, Document, bool, i32); 5] = [
            (
                "u1",
                doc! { "x": 1 },
                doc! { "$set": { "_id": [1, 2] } },
                true,
                54,
            ),
            (
                "u2",
                doc! { "_id": 1 },
                doc! { "$set": { "_id": [1, 2] } },
                true,
                54,
            ),
            (
                "u3",
                doc! { "x": 2 },
                doc! { "$set": { "_id": [1, 2] } },
                false,
                53,
            ),
            (
                "u4",
                doc! { "x": 2 },
                doc! { "$set": { "_id": { "$literal": { "$a": 1 } } } },
                false,
                52,
            ),
            (
                "u5",
                doc! { "x": 2 },
                doc! { "$set": { "_id": 7 } },
                false,
                66,
            ),
        ];
        for (coll, q, stage, upsert, want) in cases {
            dispatch_full(
                &doc! { "insert": coll, "documents": [{ "_id": 5, "x": 2 }] },
                c,
            );
            let reply = dispatch_full(
                &doc! { "update": coll, "updates": [{ "q": q, "u": [stage], "upsert": upsert }] },
                c,
            );
            assert_eq!(write_error(&reply).0, want, "{coll}: {reply}");
        }
    });
}

fn deleted_ids(c: &mut CommandContext, setup: &[Document], delete: Document) -> Vec<i32> {
    c.replica_set_name = Some("rs0".into());
    for cmd in setup {
        dispatch_full(cmd, c);
    }
    let opened = dispatch_full(
        &doc! { "aggregate": "d", "pipeline": [{ "$changeStream": {} }], "cursor": {} },
        c,
    );
    let id = opened
        .get_document("cursor")
        .unwrap()
        .get_i64("id")
        .unwrap();
    let reply = dispatch_full(&delete, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    let more = dispatch_full(
        &doc! { "getMore": id, "collection": "d", "maxTimeMS": 200 },
        c,
    );
    more.get_document("cursor")
        .unwrap()
        .get_array("nextBatch")
        .unwrap()
        .iter()
        .map(|e| e.as_document().unwrap())
        .filter(|e| e.get_str("operationType") == Ok("delete"))
        .map(|e| {
            e.get_document("documentKey")
                .unwrap()
                .get_i32("_id")
                .unwrap()
        })
        .collect()
}

/// RM-077: an unbounded delete removes its matches in mongod's batches --
/// ten at a time in its plan's order, each batch last-read first -- as its
/// change stream shows. One of ten, then the eleventh and twelfth.
#[test]
fn an_unbounded_delete_removes_in_reversed_batches() {
    with_wt(|c| {
        let docs: Vec<Bson> = (1..=12)
            .map(|i| Bson::Document(doc! { "_id": i, "a": i % 3 }))
            .collect();
        let got = deleted_ids(
            c,
            &[doc! { "insert": "d", "documents": docs }],
            doc! { "delete": "d", "deletes": [{ "q": {}, "limit": 0 }] },
        );
        assert_eq!(got, [10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 12, 11]);
    });
    // Through an index: its order, `a` then RecordId, reversed.
    with_wt(|c| {
        let docs: Vec<Bson> = (1..=5)
            .map(|i| Bson::Document(doc! { "_id": i, "a": i % 3 }))
            .collect();
        let got = deleted_ids(
            c,
            &[
                doc! { "insert": "d", "documents": docs },
                doc! { "createIndexes": "d", "indexes": [{ "key": { "a": 1 }, "name": "a_1" }] },
            ],
            doc! { "delete": "d", "deletes": [{ "q": {}, "limit": 0, "hint": "a_1" }] },
        );
        assert_eq!(got, [5, 2, 4, 1, 3]);
    });
}

/// RM-061: `$text` with no text index is mongod's 27 from every command
/// that takes a query, a write statement's included.
#[test]
fn text_without_a_text_index_is_27() {
    with_wt(|c| {
        dispatch_full(
            &doc! { "insert": "t", "documents": [{ "_id": 1, "s": "hi" }] },
            c,
        );
        let text = doc! { "$text": { "$search": "hi" } };
        let update = dispatch_full(
            &doc! { "update": "t", "updates": [{ "q": text.clone(), "u": { "$set": { "b": 1 } } }] },
            c,
        );
        assert_eq!(
            write_error(&update),
            (27, "text index required for $text query".to_string())
        );
        for cmd in [
            doc! { "find": "t", "filter": text.clone() },
            doc! { "findAndModify": "t", "query": text.clone(), "update": { "$set": { "b": 1 } } },
            doc! { "aggregate": "t", "pipeline": [{ "$match": text.clone() }], "cursor": {} },
        ] {
            assert_eq!(code(&dispatch_full(&cmd, c)), 27, "{cmd}");
        }
    });
}

/// RM-062: `$$CLUSTER_TIME` is the operation's cluster time on a
/// replica-set member, and 10071200 on a standalone.
#[test]
fn cluster_time_in_a_pipeline() {
    with_wt(|c| {
        dispatch_full(&doc! { "insert": "t", "documents": [{ "_id": 1 }] }, c);
        let set = doc! { "update": "t", "updates": [{ "q": { "_id": 1 }, "u": [{ "$set": { "t": "$$CLUSTER_TIME" } }] }] };
        c.replica_set_name = None;
        assert_eq!(write_error(&dispatch_full(&set, c)).0, 10071200);
        let agg = doc! { "aggregate": "t", "pipeline": [{ "$project": { "t": "$$CLUSTER_TIME" } }], "cursor": {} };
        assert_eq!(code(&dispatch_full(&agg, c)), 10071200);
        c.replica_set_name = Some("rs0".into());
        let reply = dispatch_full(&set, c);
        assert_eq!(reply.get_i32("nModified"), Ok(1), "{reply}");
        let found = dispatch_full(&doc! { "find": "t" }, c);
        assert!(matches!(
            first_batch(&found)[0].get("t"),
            Some(Bson::Timestamp(_))
        ));
        assert!(matches!(
            first_batch(&dispatch_full(&agg, c))[0].get("t"),
            Some(Bson::Timestamp(_))
        ));
    });
}

/// RM-063: an update's `$replaceWith` of a non-document names the whole
/// input document; an aggregate's names only the fields it read.
#[test]
fn replace_with_a_non_document_names_the_input() {
    with_wt(|c| {
        dispatch_full(
            &doc! { "insert": "t", "documents": [{ "_id": 1, "a": 5, "b": 2 }] },
            c,
        );
        let reply = dispatch_full(
            &doc! { "update": "t", "updates": [{ "q": { "_id": 1 }, "u": [{ "$replaceWith": "$a" }] }] },
            c,
        );
        let (code_, errmsg) = write_error(&reply);
        assert_eq!(code_, 40228);
        assert!(
            errmsg.ends_with("Input document: {_id: 1, a: 5, b: 2}"),
            "{errmsg}"
        );
        let agg = dispatch_full(
            &doc! { "aggregate": "t", "pipeline": [{ "$replaceWith": "$a" }], "cursor": {} },
            c,
        );
        assert!(
            agg.get_str("errmsg")
                .unwrap()
                .ends_with("Input document: {a: 5}"),
            "{agg}"
        );
    });
}

/// A `mapReduce` this server cannot evaluate (it has no JavaScript engine)
/// is refused as a mongod started with `--noscripting` refuses it (31264,
/// measured 8.2.11); it answered `ok: 1` with no results, a silent wrong
/// answer. The one recognised count shape still runs.
#[test]
fn map_reduce_without_javascript_is_refused() {
    with_wt(|c| {
        dispatch_full(
            &doc! { "insert": "m", "documents": [{ "x": 1 }, { "x": 1 }] },
            c,
        );
        let refused = dispatch_full(
            &doc! {
                "mapReduce": "m",
                "map": "function() { emit(this.x, 1); }",
                "reduce": "function(k, v) { return Array.sum(v); }",
                "out": { "inline": 1 },
            },
            c,
        );
        assert_eq!(code(&refused), 31264);
        let counted = dispatch_full(
            &doc! {
                "mapReduce": "m",
                "map": "function() { emit(this.x, 1); }",
                "reduce": "function(k, values) { return values.length; }",
                "out": { "inline": 1 },
            },
            c,
        );
        assert_eq!(counted.get_f64("ok"), Ok(1.0), "{counted}");
    });
}
