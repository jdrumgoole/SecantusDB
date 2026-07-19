//! Real-WiredTiger ports of the `find` command unit tests. Docs are seeded with
//! real `insert` commands; unsorted reads come back in natural (insertion)
//! order, exactly as the handler-plumbing tests expect.

mod common;
use common::dispatch_full;

use bson::doc;
use common::with_wt;
use secantus_commands::dispatch;

fn batch_ids(cursor: &bson::Document, key: &str) -> Vec<i64> {
    cursor
        .get_array(key)
        .unwrap()
        .iter()
        .map(|b| b.as_document().unwrap().get_i32("_id").unwrap() as i64)
        .collect()
}

fn seed(c: &mut secantus_commands::CommandContext, docs: Vec<bson::Document>) {
    dispatch(
        &doc! {"insert": "c", "documents": docs.into_iter().map(bson::Bson::Document).collect::<Vec<_>>()},
        c,
    );
}

#[test]
fn find_all_single_batch() {
    with_wt(|c| {
        seed(c, (0..3).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c"}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(cur.get_i64("id").unwrap(), 0, "all fit ⇒ no cursor");
        assert_eq!(cur.get_str("ns").unwrap(), "t.c");
        assert_eq!(batch_ids(cur, "firstBatch"), vec![0, 1, 2]);
    });
}

#[test]
fn find_non_numeric_batch_size_is_type_mismatch() {
    with_wt(|c| {
        seed(c, vec![doc! {"_id": 1}]);
        let reply = dispatch_full(&doc! {"find": "c", "batchSize": "foo"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 14);
        assert_eq!(reply.get_str("codeName").unwrap(), "TypeMismatch");
        // A numeric batchSize still works.
        let reply = dispatch_full(&doc! {"find": "c", "batchSize": 5i32}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn find_skip_and_limit() {
    with_wt(|c| {
        seed(c, (0..5).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c", "skip": 1, "limit": 2}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![1, 2]);
    });
}

#[test]
fn find_sort_descending() {
    with_wt(|c| {
        seed(c, (0..3).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c", "sort": {"_id": -1}}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![2, 1, 0]);
    });
}

#[test]
fn find_return_key_and_show_record_id() {
    with_wt(|c| {
        seed(
            c,
            (1..4)
                .map(|i| doc! {"_id": i, "x": i * 10, "y": "z"})
                .collect(),
        );
        // returnKey (+ showRecordId, which it suppresses): only the sort key field.
        let reply = dispatch_full(
            &doc! {"find": "c", "sort": {"_id": 1}, "returnKey": true, "showRecordId": true},
            c,
        );
        let batch = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        for b in batch {
            let d = b.as_document().unwrap();
            assert_eq!(
                d.keys().collect::<Vec<_>>(),
                vec!["_id"],
                "returnKey ⇒ key only"
            );
        }
        assert_eq!(batch.len(), 3);
        // showRecordId alone: full doc plus a $recordId.
        let reply = dispatch_full(
            &doc! {"find": "c", "sort": {"_id": 1}, "showRecordId": true},
            c,
        );
        let first = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert!(first.contains_key("$recordId"));
        assert!(first.contains_key("x"));
    });
}

#[test]
fn find_batched_opens_cursor_and_getmore_drains() {
    with_wt(|c| {
        seed(c, (0..5).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c", "batchSize": 2}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![0, 1]);
        let cid = cur.get_i64("id").unwrap();
        assert_ne!(cid, 0, "remaining docs ⇒ live cursor");

        let reply = dispatch_full(&doc! {"getMore": cid, "collection": "c", "batchSize": 2}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "nextBatch"), vec![2, 3]);
        assert_eq!(cur.get_i64("id").unwrap(), cid);

        let reply = dispatch_full(&doc! {"getMore": cid, "collection": "c", "batchSize": 2}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "nextBatch"), vec![4]);
        assert_eq!(cur.get_i64("id").unwrap(), 0, "exhausted");
    });
}

#[test]
fn find_batch_size_zero_empty_first_batch() {
    with_wt(|c| {
        seed(c, (0..2).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c", "batchSize": 0}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert!(cur.get_array("firstBatch").unwrap().is_empty());
        assert_ne!(cur.get_i64("id").unwrap(), 0);
    });
}

#[test]
fn find_single_batch_never_opens_cursor() {
    with_wt(|c| {
        seed(c, (0..5).map(|i| doc! {"_id": i}).collect());
        let reply = dispatch_full(&doc! {"find": "c", "batchSize": 2, "singleBatch": true}, c);
        let cur = reply.get_document("cursor").unwrap();
        // singleBatch overrides batchSize splitting: all docs, id 0.
        assert_eq!(batch_ids(cur, "firstBatch").len(), 5);
        assert_eq!(cur.get_i64("id").unwrap(), 0);
    });
}

#[test]
fn find_projection_includes_fields() {
    with_wt(|c| {
        seed(c, vec![doc! {"_id": 1, "a": 10, "b": 20}]);
        let reply = dispatch_full(&doc! {"find": "c", "projection": {"a": 1}}, c);
        let cur = reply.get_document("cursor").unwrap();
        let first = cur.get_array("firstBatch").unwrap()[0]
            .as_document()
            .unwrap();
        assert!(first.get("a").is_some());
        assert!(first.get("b").is_none(), "b excluded by projection");
        assert!(first.get("_id").is_some(), "_id included by default");
    });
}

#[test]
fn find_mixed_projection_is_rejected() {
    // A projection mixing inclusion and exclusion (except _id) is rejected with
    // mongod's per-field 31254 / 31253 — mongo-node-driver projection-error tests.
    with_wt(|c| {
        seed(c, vec![doc! {"_id": 1, "a": 1, "b": 2}]);
        let r = dispatch_full(&doc! {"find": "c", "projection": {"a": 1, "b": 0}}, c);
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 31254);
        assert!(r
            .get_str("errmsg")
            .unwrap()
            .contains("Cannot do exclusion on field b in inclusion projection"));
        // Exclusion mode + an inclusion field → 31253.
        let r = dispatch_full(&doc! {"find": "c", "projection": {"a": 0, "b": 1}}, c);
        assert_eq!(r.get_i32("code").unwrap(), 31253);
        // `_id` is exempt: {_id: 0, a: 1} is a valid inclusion projection.
        let r = dispatch_full(&doc! {"find": "c", "projection": {"_id": 0, "a": 1}}, c);
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn find_all_with_regex_elements() {
    // $all with regex elements matches array elements as patterns
    // (mongo-node-driver "Find should correctly find documents by regExp").
    with_wt(|c| {
        seed(
            c,
            vec![doc! {"_id": 1, "keywords": [
                "test", "segmentation", "fault", "regex", "serialization", "native"
            ]}],
        );
        let re = |p: &str| {
            bson::Bson::RegularExpression(bson::Regex {
                pattern: p.into(),
                options: String::new(),
            })
        };
        let r = dispatch_full(
            &doc! {"find": "c", "filter": {"keywords": {"$all": [
                re("ser"), re("test"), re("seg"), re("fault"), re("nat")
            ]}}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert_eq!(
            r.get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .len(),
            1
        );
        // A regex that matches nothing → no result.
        let r = dispatch_full(
            &doc! {"find": "c", "filter": {"keywords": {"$all": [re("zzz")]}}},
            c,
        );
        assert_eq!(
            r.get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .len(),
            0
        );
    });
}

#[test]
fn find_geo_center_near_nearsphere() {
    // Mirrors mongo-java-driver GeoFiltersFunctionalSpecification: a 2d index over
    // legacy [x,y] points, queried with $geoWithin $center, legacy-sibling $near,
    // and $nearSphere; results sorted by _id.
    with_wt(|c| {
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"geo": "2d"}, "name": "geo_2d"}]},
            c,
        );
        seed(
            c,
            vec![
                doc! {"_id": 1, "geo": [1.0, 1.0]},
                doc! {"_id": 2, "geo": [45.0, 2.0]},
                doc! {"_id": 3, "geo": [3.0, 3.0]},
            ],
        );
        let ids = |r: &bson::Document| -> Vec<i64> {
            batch_ids(r.get_document("cursor").unwrap(), "firstBatch")
        };
        // $geoWithin $center [[2,2], 4] -> points 1 and 3 (within dist 4 of (2,2)).
        let r = dispatch_full(
            &doc! {"find": "c", "filter": {"geo": {"$geoWithin": {"$center": [[2.0, 2.0], 4.0]}}}, "sort": {"_id": 1}},
            c,
        );
        assert_eq!(ids(&r), vec![1, 3], "$center {r:?}");
        // legacy 2d $near with sibling $maxDistance -> only point 1.
        let r = dispatch_full(
            &doc! {"find": "c", "filter": {"geo": {"$near": [1.01, 1.01], "$maxDistance": 0.1, "$minDistance": 0.0}}, "sort": {"_id": 1}},
            c,
        );
        assert_eq!(ids(&r), vec![1], "$near {r:?}");
        // $nearSphere (radians bound 0.1) -> points 1 and 3.
        let r = dispatch_full(
            &doc! {"find": "c", "filter": {"geo": {"$nearSphere": [1.01, 1.01], "$maxDistance": 0.1, "$minDistance": 0.0}}, "sort": {"_id": 1}},
            c,
        );
        assert_eq!(ids(&r), vec![1, 3], "$nearSphere {r:?}");
    });
}

#[test]
fn find_filter_matches_subset() {
    with_wt(|c| {
        seed(
            c,
            vec![
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 2},
                doc! {"_id": 3, "x": 1},
            ],
        );
        let reply = dispatch_full(&doc! {"find": "c", "filter": {"x": 1}}, c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![1, 3]);
    });
}
