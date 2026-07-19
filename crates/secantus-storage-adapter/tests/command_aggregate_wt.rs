//! Real-WiredTiger ports of the `aggregate` command unit tests. Source and
//! foreign collections are seeded with real `insert` commands; `$out` / `$merge`
//! targets are verified by reading the destination back through `find`.

mod common;

use bson::{doc, Bson, Document};
use common::with_wt;
use secantus_commands::{dispatch, CommandContext};

fn first_batch(reply: &Document) -> Vec<Bson> {
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .clone()
}

fn docs_of(reply: &Document) -> Vec<Document> {
    first_batch(reply)
        .iter()
        .map(|b| b.as_document().unwrap().clone())
        .collect()
}

fn seed(c: &mut CommandContext, coll: &str, docs: Vec<Document>) {
    dispatch(
        &doc! {"insert": coll, "documents": docs.into_iter().map(Bson::Document).collect::<Vec<_>>()},
        c,
    );
}

/// The cursor batch as decoded docs. A no-projection `find` hands its batch to
/// the server as pre-encoded blobs via `ctx.pending_batch` (the raw-BSON reply
/// fast path) rather than a `firstBatch` array in the reply document; aggregate
/// and projected-find replies still carry `firstBatch` inline. Read whichever
/// the handler produced.
fn batch_docs(reply: &Document, c: &CommandContext) -> Vec<Document> {
    match &c.pending_batch {
        Some(pb) => pb
            .batch
            .iter()
            .map(|b| Document::from_reader(&mut &b[..]).unwrap())
            .collect(),
        None => docs_of(reply),
    }
}

/// All docs in `coll`, read back through a real `find`.
fn read_all(c: &mut CommandContext, coll: &str) -> Vec<Document> {
    let reply = dispatch(&doc! {"find": coll}, c);
    batch_docs(&reply, c)
}

#[test]
fn aggregate_match_then_count() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 2},
                doc! {"_id": 3, "x": 1},
            ],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$match": {"x": 1}}, {"$count": "n"}], "cursor": {}},
            c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_document().unwrap().get_i32("n").unwrap(), 2);
        assert_eq!(
            reply.get_document("cursor").unwrap().get_str("ns").unwrap(),
            "t.c"
        );
    });
}

#[test]
fn aggregate_group_sum() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "v": 10},
                doc! {"_id": 2, "v": 20},
                doc! {"_id": 3, "v": 30},
            ],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$group": {"_id": Bson::Null, "total": {"$sum": "$v"}}}
            ], "cursor": {}},
            c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_document().unwrap().get_i32("total").unwrap(), 60);
    });
}

#[test]
fn aggregate_sort_then_limit() {
    with_wt(|c| {
        seed(c, "c", (1..=4).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sort": {"_id": -1}}, {"$limit": 2}], "cursor": {}},
            c,
        );
        let ids: Vec<i32> = first_batch(&reply)
            .iter()
            .map(|b| b.as_document().unwrap().get_i32("_id").unwrap())
            .collect();
        assert_eq!(ids, vec![4, 3]);
    });
}

#[test]
fn aggregate_unrecognized_stage_is_location_40324() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1}]);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$notARealStage": {}}], "cursor": {}},
            c,
        );
        // An unrecognized stage name is validated up-front as Location40324
        // ("Unrecognized pipeline stage name"), matching mongod / the Python server.
        assert_eq!(reply.get_i32("code").unwrap(), 40324);
        assert_eq!(reply.get_str("codeName").unwrap(), "Location40324");
    });
}

#[test]
fn aggregate_list_local_sessions_source_stage() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"aggregate": 1, "pipeline": [
                {"$listLocalSessions": {}},
                {"$limit": 1},
                {"$addFields": {"dummy": "dummy field"}},
                {"$project": {"_id": 0, "dummy": 1}},
            ], "cursor": {}},
            c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(
            fb[0].as_document().unwrap().get_str("dummy").unwrap(),
            "dummy field"
        );
    });
}

#[test]
fn aggregate_current_op_synthetic_shape() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"aggregate": 1, "pipeline": [{"$currentOp": {}}], "cursor": {}},
            c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        let row = fb[0].as_document().unwrap();
        assert_eq!(row.get_str("type").unwrap(), "op");
        assert_eq!(row.get_str("op").unwrap(), "command");
        let cmd = row.get_document("command").unwrap();
        assert!(cmd.contains_key("aggregate"));
        assert_eq!(cmd.get_str("$db").unwrap(), "t");
    });
}

#[test]
fn geo_near_sorts_by_distance_and_attaches_field() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "loc": [0.0, 0.0]},
                doc! {"_id": 2, "loc": [3.0, 4.0]},
                doc! {"_id": 3, "loc": [1.0, 0.0]},
            ],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$geoNear": {"near": [0.0, 0.0], "key": "loc", "distanceField": "d"}}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        let ids: Vec<i32> = out.iter().map(|d| d.get_i32("_id").unwrap()).collect();
        assert_eq!(ids, vec![1, 3, 2]);
        let dists: Vec<f64> = out.iter().map(|d| d.get_f64("d").unwrap()).collect();
        assert_eq!(dists, vec![0.0, 1.0, 5.0]);
    });
}

#[test]
fn geo_near_max_distance_and_multiplier() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "loc": [0.0, 0.0]},
                doc! {"_id": 2, "loc": [3.0, 4.0]},
            ],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$geoNear": {"near": [0.0, 0.0], "key": "loc", "distanceField": "d",
                              "maxDistance": 2.0, "distanceMultiplier": 10.0}}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_i32("_id").unwrap(), 1);
        assert_eq!(out[0].get_f64("d").unwrap(), 0.0);
    });
}

#[test]
fn aggregate_changestream_standalone_rejected() {
    with_wt(|c| {
        // replica_set_name is None ⇒ standalone.
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 40573);
        assert_eq!(reply.get_str("codeName").unwrap(), "IllegalOperation");
    });
}

#[test]
fn change_stream_invalid_match_errors_at_open() {
    with_wt(|c| {
        // Change streams require a replica-set persona.
        c.replica_set_name = Some("secantus".into());
        // Unknown operator in $match → error at aggregate (.begin()) time, not
        // lazily at the first getMore (mongo-cxx-driver invalid-pipeline test).
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$changeStream": {}}, {"$match": {"$foo": -1}}
            ], "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 2);
        // A valid $match opens the stream fine (live cursor returned).
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$changeStream": {}}, {"$match": {"operationType": "insert"}}
            ], "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert!(r.get_document("cursor").is_ok());
    });
}

#[test]
fn lookup_simple_form_joins_foreign_docs() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![doc! {"_id": 1, "k": 10}, doc! {"_id": 2, "k": 20}],
        );
        seed(
            c,
            "o",
            vec![
                doc! {"_id": 100, "fk": 10, "v": "a"},
                doc! {"_id": 101, "fk": 10, "v": "b"},
                doc! {"_id": 102, "fk": 20, "v": "c"},
            ],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$lookup": {"from": "o", "localField": "k", "foreignField": "fk", "as": "j"}},
                {"$sort": {"_id": 1}}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        assert_eq!(out[0].get_array("j").unwrap().len(), 2);
        assert_eq!(out[1].get_array("j").unwrap().len(), 1);
        assert_eq!(
            out[1].get_array("j").unwrap()[0]
                .as_document()
                .unwrap()
                .get_str("v")
                .unwrap(),
            "c"
        );
    });
}

#[test]
fn lookup_pipeline_form_with_let_binding() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1, "k": 5}]);
        seed(
            c,
            "o",
            vec![doc! {"_id": 1, "n": 3}, doc! {"_id": 2, "n": 9}],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$lookup": {
                    "from": "o",
                    "let": {"kk": "$k"},
                    "pipeline": [{"$match": {"$expr": {"$gt": ["$n", "$$kk"]}}}],
                    "as": "j"
                }}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        let j = out[0].get_array("j").unwrap();
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].as_document().unwrap().get_i32("n").unwrap(), 9);
    });
}

#[test]
fn sample_returns_requested_size_subset() {
    with_wt(|c| {
        seed(c, "c", (0..10).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sample": {"size": 3}}], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 3);
        let mut ids: Vec<i32> = out.iter().map(|d| d.get_i32("_id").unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
    });
}

#[test]
fn sample_size_ge_len_returns_all() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1}, doc! {"_id": 2}]);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sample": {"size": 50}}], "cursor": {}},
            c,
        );
        assert_eq!(docs_of(&reply).len(), 2);
    });
}

#[test]
fn coll_stats_reports_count_and_index_sizes() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![doc! {"_id": 1}, doc! {"_id": 2}, doc! {"_id": 3}],
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$collStats": {"storageStats": {}, "count": {}}}], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_str("ns").unwrap(), "t.c");
        let ss = out[0].get_document("storageStats").unwrap();
        assert_eq!(ss.get_i64("count").unwrap(), 3);
        assert_eq!(ss.get_i32("nindexes").unwrap(), 1);
        assert!(ss.get_document("indexSizes").unwrap().contains_key("_id_"));
        assert_eq!(out[0].get_i64("count").unwrap(), 3);
    });
}

#[test]
fn index_stats_one_doc_per_index() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1}]);
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"x": 1}, "name": "x_1"}]},
            c,
        );
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$indexStats": {}}], "cursor": {}},
            c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|d| d.get_str("name").unwrap()).collect();
        assert!(names.contains(&"_id_") && names.contains(&"x_1"));
    });
}

#[test]
fn out_replaces_target_collection() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![doc! {"_id": 1, "v": 1}, doc! {"_id": 2, "v": 2}],
        );
        // Pre-existing junk in the target must be wiped by $out.
        seed(c, "dst", vec![doc! {"_id": 99, "stale": true}]);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$out": "dst"}], "cursor": {}},
            c,
        );
        assert_eq!(docs_of(&reply).len(), 0, "$out emits nothing downstream");
        let mut ids: Vec<i32> = read_all(c, "dst")
            .iter()
            .map(|d| d.get_i32("_id").unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    });
}

#[test]
fn out_or_merge_not_last_stage_is_rejected() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1}]);
        // $out before the end → Location40601, nothing written.
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$out": "dst"}, {"$match": {}}], "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 40601);
        assert_eq!(r.get_str("codeName").unwrap(), "Location40601");
        // $merge non-terminal too.
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$merge": {"into": "dst"}}, {"$limit": 1}], "cursor": {}},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 40601);
        // Target was never created (rejected before executing any stage).
        assert!(read_all(c, "dst").is_empty());
    });
}

#[test]
fn out_enforces_target_validator_unless_bypassed() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1, "n": 5}]);
        dispatch(&doc! {"create": "dst", "validator": {"n": {"$gt": 100}}}, c);

        // n=5 fails the validator → 121.
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$out": "dst"}], "cursor": {}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 121);
        assert_eq!(
            reply.get_str("codeName").unwrap(),
            "DocumentValidationFailure"
        );

        // bypassDocumentValidation: true → the write goes through.
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$out": "dst"}],
            "bypassDocumentValidation": true, "cursor": {}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(read_all(c, "dst").len(), 1);
    });
}

#[test]
fn merge_deep_merges_matched_and_inserts_unmatched() {
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "a": {"x": 1}, "new": "f"},
                doc! {"_id": 2, "fresh": true},
            ],
        );
        seed(c, "dst", vec![doc! {"_id": 1, "a": {"y": 2}, "keep": "g"}]);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$merge": {"into": "dst"}}], "cursor": {}},
            c,
        );
        assert_eq!(docs_of(&reply).len(), 0);
        let dst = read_all(c, "dst");
        let merged = dst.iter().find(|d| d.get_i32("_id") == Ok(1)).unwrap();
        // deep merge: existing a.y kept, new a.x added, keep retained, new added.
        let a = merged.get_document("a").unwrap();
        assert_eq!(a.get_i32("x").unwrap(), 1);
        assert_eq!(a.get_i32("y").unwrap(), 2);
        assert_eq!(merged.get_str("keep").unwrap(), "g");
        assert_eq!(merged.get_str("new").unwrap(), "f");
        assert!(
            dst.iter().any(|d| d.get_i32("_id") == Ok(2)),
            "unmatched inserted"
        );
    });
}

#[test]
fn merge_keep_existing_skips_matched() {
    with_wt(|c| {
        seed(c, "c", vec![doc! {"_id": 1, "v": "new"}]);
        seed(c, "dst", vec![doc! {"_id": 1, "v": "old"}]);
        dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$merge": {"into": "dst", "whenMatched": "keepExisting"}}
            ], "cursor": {}},
            c,
        );
        let dst = read_all(c, "dst");
        assert_eq!(dst[0].get_str("v").unwrap(), "old");
    });
}

#[test]
fn aggregate_explain_option_returns_plan_without_running() {
    // The inline `explain: true` aggregate flag (mongo-php-library
    // AggregateFunctionalTest::testExplainOption*) returns the plan instead of a
    // cursor and must NOT execute a write stage.
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![doc! {"_id": 1}, doc! {"_id": 2}, doc! {"_id": 3}],
        );
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$match": {"_id": {"$ne": 2}}}], "explain": true},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert!(r.get_array("stages").is_ok(), "explain output has stages");
        assert!(r.get_document("queryPlanner").is_ok());

        // $out as the final stage with explain:true returns stages and is a dry
        // run — the target collection is never written.
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$match": {"_id": {"$ne": 2}}}, {"$out": "c.output"}
            ], "explain": true},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert!(r.get_array("stages").is_ok());
        assert!(
            read_all(c, "c.output").is_empty(),
            "$out not executed under explain"
        );
    });
}

#[test]
fn bucket_auto_chunks_by_count() {
    // php-lib Builder{Collection,Database}FunctionalTest::testAggregate. Pure
    // count-chunking (Python parity): 3 docs / 2 buckets → chunks of 1 then 2.
    with_wt(|c| {
        // Collection variant: 3 identical values still split into 2 buckets.
        seed(c, "c", vec![doc! {"x": 10}, doc! {"x": 10}, doc! {"x": 10}]);
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$bucketAuto": {"groupBy": "$x", "buckets": 2}}
            ], "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert_eq!(docs_of(&r).len(), 2);

        // Database variant: $documents source + $bucketAuto over distinct values.
        let r = dispatch(
            &doc! {"aggregate": 1, "pipeline": [
                {"$documents": [{"x": 1}, {"x": 2}, {"x": 3}]},
                {"$bucketAuto": {"groupBy": "$x", "buckets": 2}}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&r);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].get_i32("count").unwrap(), 1);
        assert_eq!(out[1].get_i32("count").unwrap(), 2);
        assert_eq!(
            out[0].get_document("_id").unwrap().get_i32("min").unwrap(),
            1
        );
    });
}

#[test]
fn fill_locf_over_sorted_docs() {
    // $fill locf carries the last observed value forward over the sorted docs.
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![
                doc! {"_id": 1, "t": 1, "v": 10},
                doc! {"_id": 2, "t": 2},
                doc! {"_id": 3, "t": 3, "v": 30},
                doc! {"_id": 4, "t": 4},
            ],
        );
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "locf"}}}}
            ], "cursor": {}},
            c,
        );
        let vs: Vec<i32> = docs_of(&r)
            .iter()
            .map(|d| d.get_i32("v").unwrap())
            .collect();
        assert_eq!(vs, vec![10, 10, 30, 30], "{r:?}");
    });
}

#[test]
fn union_with_concatenates_collections() {
    // $unionWith appends docs from another collection (bare-name and
    // {coll, pipeline} forms), input docs first.
    with_wt(|c| {
        seed(
            c,
            "a",
            vec![doc! {"_id": 1, "src": "a"}, doc! {"_id": 2, "src": "a"}],
        );
        seed(
            c,
            "b",
            vec![doc! {"_id": 10, "n": 5}, doc! {"_id": 11, "n": 1}],
        );
        let ids = |r: &Document| -> Vec<i32> {
            docs_of(r)
                .iter()
                .map(|d| d.get_i32("_id").unwrap())
                .collect()
        };
        // Bare form: all of a, then all of b.
        let r = dispatch(
            &doc! {"aggregate": "a", "pipeline": [{"$unionWith": "b"}], "cursor": {}},
            c,
        );
        assert_eq!(ids(&r), vec![1, 2, 10, 11], "{r:?}");
        // {coll, pipeline} form: b filtered by a sub-pipeline.
        let r = dispatch(
            &doc! {"aggregate": "a", "pipeline": [
                {"$unionWith": {"coll": "b", "pipeline": [{"$match": {"n": {"$gt": 1}}}]}}
            ], "cursor": {}},
            c,
        );
        assert_eq!(ids(&r), vec![1, 2, 10], "filtered union {r:?}");
    });
}

#[test]
fn redact_prunes_and_descends() {
    // $redact with $$PRUNE/$$DESCEND prunes a high-level sub-doc, keeps the rest
    // (mongo-cxx-driver redact/aggregation).
    with_wt(|c| {
        seed(
            c,
            "c",
            vec![doc! {
                "_id": 1, "level": 1,
                "secret": {"level": 5, "data": "x"},
                "public": {"level": 1, "data": "y"},
            }],
        );
        let r = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$redact": {"$cond": {
                    "if": {"$gt": ["$level", 3]},
                    "then": "$$PRUNE",
                    "else": "$$DESCEND"
                }}}
            ], "cursor": {}},
            c,
        );
        let out = docs_of(&r);
        assert_eq!(out.len(), 1);
        // secret (level 5) pruned; public (level 1) kept; scalars retained.
        assert!(out[0].get("secret").is_none());
        assert!(out[0].get_document("public").is_ok());
        assert_eq!(out[0].get_i32("level").unwrap(), 1);
    });
}

#[test]
fn aggregate_batches_into_cursor() {
    with_wt(|c| {
        seed(c, "c", (0..5).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sort": {"_id": 1}}], "cursor": {"batchSize": 2}},
            c,
        );
        let cursor = reply.get_document("cursor").unwrap();
        assert_eq!(cursor.get_array("firstBatch").unwrap().len(), 2);
        assert_ne!(cursor.get_i64("id").unwrap(), 0, "remaining ⇒ live cursor");
    });
}
