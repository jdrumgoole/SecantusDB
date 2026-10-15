//! The `notablescan` server parameter over real WiredTiger: a query whose
//! plan would scan the collection is refused with 291. Every expectation is
//! what mongod 8.2.11 answered, as a one-member replica set, on 2026-10-15.

mod common;

use std::sync::Arc;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::params::ServerParams;
use secantus_commands::CommandContext;

const UNINDEXED: &str = "No indexed plans available, and running with 'notablescan' 2";
const NATURAL: &str = "could not force a collection scan with a $natural hint :: caused by :: \
                       not allowed to output a collection scan because 'notablescan' is enabled";

/// Run `cmd` on `db`.
fn on(db: &str, cmd: &Document, c: &mut CommandContext) -> Document {
    let previous = std::mem::replace(&mut c.db_name, db.into());
    let reply = dispatch_full(cmd, c);
    c.db_name = previous;
    reply
}

fn set(on_off: impl Into<Bson>, c: &mut CommandContext) -> Document {
    on(
        "admin",
        &doc! {"setParameter": 1, "notablescan": on_off.into()},
        c,
    )
}

/// `t.c`: five documents, an index on `a` and none on `b`.
fn seeded(c: &mut CommandContext) {
    c.server_params = Some(Arc::new(ServerParams::new()));
    let docs: Vec<Document> = (0..5).map(|i| doc! {"_id": i, "a": i, "b": i}).collect();
    on("t", &doc! {"insert": "c", "documents": docs}, c);
    on(
        "t",
        &doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
        c,
    );
    set(true, c);
}

fn ok(reply: &Document) -> bool {
    reply.get_f64("ok") == Ok(1.0)
}

/// The refusal's code and closing words, with the namespace it names.
fn refused(reply: &Document, ns: &str, words: &str) {
    assert_eq!(reply.get_i32("code"), Ok(291), "{reply}");
    assert_eq!(reply.get_str("codeName"), Ok("NoQueryExecutionPlans"));
    assert_eq!(
        reply.get_str("errmsg").unwrap(),
        format!("error processing query: ns={ns} planner returned error :: caused by :: {words}")
    );
}

fn found(reply: &Document) -> usize {
    reply
        .get_document("cursor")
        .and_then(|c| c.get_array("firstBatch"))
        .unwrap_or_else(|_| panic!("{reply}"))
        .len()
}

#[test]
fn get_and_set_parameter() {
    with_wt(|c| {
        c.server_params = Some(Arc::new(ServerParams::new()));
        let got = on("admin", &doc! {"getParameter": 1, "notablescan": 1}, c);
        assert_eq!(got.get_bool("notablescan"), Ok(false));
        let got = on(
            "admin",
            &doc! {"getParameter": {"showDetails": true}, "notablescan": 1},
            c,
        );
        assert_eq!(
            got.get_document("notablescan").unwrap(),
            &doc! {"value": false, "settableAtRuntime": true, "settableAtStartup": true}
        );
        // A bool parameter: any value, stored as its truthiness.
        for (value, was, now) in [
            (Bson::Boolean(true), false, true),
            (Bson::String(String::new()), true, true),
            (Bson::Null, true, false),
            (Bson::Int32(1), false, true),
            (Bson::Double(0.0), true, false),
            (Bson::Array(vec![]), false, true),
        ] {
            let reply = set(value, c);
            assert_eq!(reply.get_bool("was"), Ok(was), "{reply}");
            let got = on("admin", &doc! {"getParameter": 1, "notablescan": 1}, c);
            assert_eq!(got.get_bool("notablescan"), Ok(now));
        }
    });
}

#[test]
fn find_is_refused_only_when_its_plan_scans_the_collection() {
    with_wt(|c| {
        seeded(c);
        refused(
            &on("t", &doc! {"find": "c", "filter": {"b": 1}}, c),
            "t.c",
            UNINDEXED,
        );
        assert_eq!(
            found(&on("t", &doc! {"find": "c", "filter": {"a": 1}}, c)),
            1
        );
        assert_eq!(found(&on("t", &doc! {"find": "c", "filter": {}}, c)), 5);
        assert_eq!(
            found(&on("t", &doc! {"find": "c", "filter": {"_id": 1}}, c)),
            1
        );
        // An empty filter runs, sorted on an unindexed field or not.
        assert_eq!(
            found(&on(
                "t",
                &doc! {"find": "c", "filter": {}, "sort": {"b": 1}},
                c
            )),
            5
        );
        // A sort the index returns serves an unindexed filter.
        assert_eq!(
            found(&on(
                "t",
                &doc! {"find": "c", "filter": {"b": 1}, "sort": {"a": 1}},
                c
            )),
            1
        );
        // A hinted index is read, whatever the filter.
        assert_eq!(
            found(&on(
                "t",
                &doc! {"find": "c", "filter": {"b": 1}, "hint": {"a": 1}},
                c
            )),
            1
        );
        // A `$natural` hint with a filter is refused in words of its own.
        refused(
            &on(
                "t",
                &doc! {"find": "c", "filter": {"a": 1}, "hint": {"$natural": 1}},
                c,
            ),
            "t.c",
            NATURAL,
        );
        assert_eq!(
            found(&on(
                "t",
                &doc! {"find": "c", "filter": {}, "hint": {"$natural": -1}},
                c
            )),
            5
        );
        // An `$or` with a branch no index serves; a filter that only looks
        // empty.
        for filter in [
            doc! {"$or": [{"a": 1}, {"b": 1}]},
            doc! {"$comment": "x"},
            doc! {"$and": [{}]},
            doc! {"$where": "true"},
        ] {
            refused(
                &on("t", &doc! {"find": "c", "filter": filter}, c),
                "t.c",
                UNINDEXED,
            );
        }
        // A predicate that cannot match needs no scan.
        assert_eq!(
            found(&on(
                "t",
                &doc! {"find": "c", "filter": {"$alwaysFalse": 1}},
                c
            )),
            0
        );
        // A collection that does not exist answers nothing.
        assert_eq!(
            found(&on("t", &doc! {"find": "nope", "filter": {"b": 1}}, c)),
            0
        );
        // Off again, everything runs.
        set(false, c);
        assert_eq!(
            found(&on("t", &doc! {"find": "c", "filter": {"b": 1}}, c)),
            1
        );
    });
}

#[test]
fn an_expr_compared_with_a_constant_rides_its_index() {
    with_wt(|c| {
        seeded(c);
        let filter = doc! {"$expr": {"$gt": ["$a", 1]}};
        assert_eq!(found(&on("t", &doc! {"find": "c", "filter": filter}, c)), 3);
        for filter in [
            doc! {"$expr": {"$eq": ["$b", 1]}},
            doc! {"$expr": {"$ne": ["$a", 1]}},
            doc! {"$expr": {"$eq": [{"$add": ["$a", 1]}, 2]}},
        ] {
            refused(
                &on("t", &doc! {"find": "c", "filter": filter}, c),
                "t.c",
                UNINDEXED,
            );
        }
    });
}

#[test]
fn the_namespaces_mongod_exempts() {
    with_wt(|c| {
        seeded(c);
        for (db, coll) in [
            ("admin", "u"),
            ("config", "u"),
            ("local", "u"),
            ("t", "system.js"),
        ] {
            on(db, &doc! {"insert": coll, "documents": [{"b": 1}]}, c);
            let reply = on(db, &doc! {"find": coll, "filter": {"b": 1}}, c);
            assert_eq!(found(&reply), 1, "{db}.{coll}: {reply}");
        }
        // A time-series collection is read through `system.buckets.*`.
        on(
            "t",
            &doc! {"create": "ts", "timeseries": {"timeField": "t"}},
            c,
        );
        on(
            "t",
            &doc! {"insert": "ts", "documents": [{"t": bson::DateTime::now(), "b": 1}]},
            c,
        );
        assert_eq!(
            found(&on("t", &doc! {"find": "ts", "filter": {"b": 1}}, c)),
            1
        );
    });
}

#[test]
fn count_distinct_and_aggregate() {
    with_wt(|c| {
        seeded(c);
        refused(
            &on("t", &doc! {"count": "c", "query": {"b": 1}}, c),
            "t.c",
            UNINDEXED,
        );
        assert_eq!(
            on("t", &doc! {"count": "c", "query": {}}, c).get_i32("n"),
            Ok(5)
        );
        refused(
            &on(
                "t",
                &doc! {"count": "c", "query": {"b": 1}, "hint": {"$natural": 1}},
                c,
            ),
            "t.c",
            NATURAL,
        );
        refused(
            &on(
                "t",
                &doc! {"distinct": "c", "key": "b", "query": {"b": 1}},
                c,
            ),
            "t.c",
            UNINDEXED,
        );
        assert!(ok(&on(
            "t",
            &doc! {"distinct": "c", "key": "b", "query": {"a": 1}},
            c
        )));
        assert!(ok(&on("t", &doc! {"distinct": "c", "key": "b"}, c)));
        refused(
            &on(
                "t",
                &doc! {"aggregate": "c", "pipeline": [{"$match": {"b": 1}}], "cursor": {}},
                c,
            ),
            "t.c",
            UNINDEXED,
        );
        // A `$match` after a `$limit` is not the query; one after a `$sort`
        // the index returns is, and the index serves it.
        for pipeline in [
            vec![doc! {"$limit": 5}, doc! {"$match": {"b": 1}}],
            vec![doc! {"$sort": {"a": 1}}, doc! {"$match": {"b": 1}}],
            vec![doc! {"$match": {"a": 1}}],
            vec![doc! {"$sort": {"b": 1}}],
        ] {
            let reply = on(
                "t",
                &doc! {"aggregate": "c", "pipeline": pipeline.clone(), "cursor": {}},
                c,
            );
            assert!(ok(&reply), "{pipeline:?}: {reply}");
        }
        // A view's pipeline is part of the query.
        on(
            "t",
            &doc! {"create": "v", "viewOn": "c", "pipeline": [{"$match": {"b": 1}}]},
            c,
        );
        refused(
            &on("t", &doc! {"find": "v", "filter": {}}, c),
            "t.c",
            UNINDEXED,
        );
    });
}

#[test]
fn a_stage_that_reads_another_collection() {
    with_wt(|c| {
        seeded(c);
        on(
            "t",
            &doc! {"insert": "f", "documents": [{"_id": 1, "b": 1}]},
            c,
        );
        let executor = |reply: &Document| {
            assert_eq!(reply.get_i32("code"), Ok(291), "{reply}");
            assert_eq!(
                reply.get_str("errmsg").unwrap(),
                format!(
                    "Executor error during aggregate command on namespace: t.c :: caused by :: \
                     error processing query: ns=t.f planner returned error :: caused by :: \
                     {UNINDEXED}"
                )
            );
        };
        let lead = doc! {"$match": {"_id": 1}};
        executor(&on(
            "t",
            &doc! {"aggregate": "c", "cursor": {}, "pipeline": [lead.clone(),
            {"$lookup": {"from": "f", "pipeline": [{"$match": {"b": 1}}], "as": "j"}}]},
            c,
        ));
        executor(&on(
            "t",
            &doc! {"aggregate": "c", "cursor": {}, "pipeline": [lead.clone(),
            {"$unionWith": {"coll": "f", "pipeline": [{"$match": {"b": 1}}]}}]},
            c,
        ));
        executor(&on(
            "t",
            &doc! {"aggregate": "c", "cursor": {}, "pipeline": [lead.clone(),
            {"$graphLookup": {"from": "f", "startWith": "$b", "connectFromField": "b",
                "connectToField": "b", "as": "g"}}]},
            c,
        ));
        // With an index on the field, each reads it; with no pipeline, a
        // `$unionWith` reads the whole collection, which is not refused.
        on(
            "t",
            &doc! {"createIndexes": "f", "indexes": [{"key": {"b": 1}, "name": "b_1"}]},
            c,
        );
        for stage in [
            doc! {"$lookup": {"from": "f", "pipeline": [{"$match": {"b": 1}}], "as": "j"}},
            doc! {"$unionWith": {"coll": "f", "pipeline": [{"$match": {"b": 1}}]}},
            doc! {"$graphLookup": {"from": "f", "startWith": "$b", "connectFromField": "b",
            "connectToField": "b", "as": "g"}},
            doc! {"$unionWith": "c"},
        ] {
            let reply = on(
                "t",
                &doc! {"aggregate": "c", "cursor": {}, "pipeline": [lead.clone(), stage.clone()]},
                c,
            );
            assert!(ok(&reply), "{stage}: {reply}");
        }
    });
}

#[test]
fn a_write_statement_is_refused_in_its_write_errors() {
    with_wt(|c| {
        seeded(c);
        let words = format!(
            "error processing query: ns=t.c planner returned error :: caused by :: {UNINDEXED}"
        );
        let reply = on(
            "t",
            &doc! {"update": "c", "ordered": false, "updates": [
                {"q": {"b": 1}, "u": {"$set": {"z": 1}}},
                {"q": {"_id": 2}, "u": {"$set": {"z": 1}}},
            ]},
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");
        assert_eq!(
            reply.get_array("writeErrors").unwrap(),
            &vec![Bson::Document(
                doc! {"index": 0, "code": 291, "errmsg": words.clone()}
            )]
        );
        let reply = on(
            "t",
            &doc! {"delete": "c", "deletes": [{"q": {"b": 1}, "limit": 0}]},
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(0), "{reply}");
        assert_eq!(
            reply.get_array("writeErrors").unwrap(),
            &vec![Bson::Document(
                doc! {"index": 0, "code": 291, "errmsg": words.clone()}
            )]
        );
        // An upsert is refused too; an empty filter is not.
        let reply = on(
            "t",
            &doc! {"update": "c", "updates": [
            {"q": {"b": 99}, "u": {"$set": {"z": 1}}, "upsert": true}]},
            c,
        );
        assert!(reply.contains_key("writeErrors"), "{reply}");
        let reply = on(
            "t",
            &doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "multi": true}]},
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(5), "{reply}");
        refused(
            &on(
                "t",
                &doc! {"findAndModify": "c", "query": {"b": 1}, "remove": true},
                c,
            ),
            "t.c",
            UNINDEXED,
        );
        assert!(ok(&on(
            "t",
            &doc! {"findAndModify": "c", "query": {"a": 1}, "update": {"$set": {"z": 2}}},
            c
        )));
        // Nothing the refused statements named was changed.
        assert_eq!(found(&on("t", &doc! {"find": "c", "filter": {}}, c)), 5);
    });
}

#[test]
fn explain_is_refused_as_the_command_is() {
    with_wt(|c| {
        seeded(c);
        for inner in [
            doc! {"find": "c", "filter": {"b": 1}},
            doc! {"count": "c", "query": {"b": 1}},
            doc! {"distinct": "c", "key": "b", "query": {"b": 1}},
            doc! {"delete": "c", "deletes": [{"q": {"b": 1}, "limit": 0}]},
            doc! {"update": "c", "updates": [{"q": {"b": 1}, "u": {"$set": {"z": 1}}}]},
            doc! {"findAndModify": "c", "query": {"b": 1}, "remove": true},
            doc! {"aggregate": "c", "pipeline": [{"$match": {"b": 1}}], "cursor": {}},
        ] {
            refused(
                &on("t", &doc! {"explain": inner.clone()}, c),
                "t.c",
                UNINDEXED,
            );
        }
        refused(
            &on(
                "t",
                &doc! {"explain": {"find": "c", "filter": {"b": 1}, "hint": {"$natural": 1}}},
                c,
            ),
            "t.c",
            NATURAL,
        );
        let reply = on("t", &doc! {"explain": {"find": "c", "filter": {"a": 1}}}, c);
        assert!(ok(&reply), "{reply}");
    });
}
