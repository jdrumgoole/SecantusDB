//! A time-series collection's buckets, and the four collection commands
//! added with them (`convertToCapped`, `cloneCollectionAsCapped`, `compact`,
//! `reIndex`). The server keeps measurements, not buckets; what it derives
//! from them is checked here against what mongod 8.2.11 answered
//! (`tools/probes/time_series.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn at(minutes: i64) -> Bson {
    // 2026-01-01T00:00:00Z
    Bson::DateTime(bson::DateTime::from_millis(
        1_767_225_600_000 + minutes * 60_000,
    ))
}

fn seed(c: &mut CommandContext) {
    let reply = dispatch(
        &doc! {"create": "w", "timeseries": {"timeField": "t", "metaField": "m"}},
        c,
    );
    assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
    dispatch(
        &doc! {"insert": "w", "documents": [
            {"_id": 1, "t": at(0), "m": "a", "v": 1},
            {"_id": 2, "t": at(1), "m": "b", "v": 2},
            {"_id": 3, "t": at(2), "m": "a", "v": 3},
            {"_id": 4, "t": at(90), "m": "a", "v": 4},
        ]},
        c,
    );
}

fn batch(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("no cursor: {reply}"))
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn ids(reply: &Document) -> Vec<i32> {
    batch(reply)
        .iter()
        .map(|d| d.get_i32("_id").unwrap())
        .collect()
}

/// mongod returns measurements bucket by bucket, each with its time and meta
/// fields first. They came back in insertion order and as written.
#[test]
fn a_scan_goes_bucket_by_bucket() {
    with_wt(|c| {
        seed(c);
        let found = dispatch_full(&doc! {"find": "w"}, c);
        assert_eq!(ids(&found), vec![1, 3, 2, 4]);
        let keys: Vec<String> = batch(&found)[0].keys().cloned().collect();
        assert_eq!(keys, vec!["t", "m", "_id", "v"]);
        let aggregated = dispatch_full(&doc! {"aggregate": "w", "pipeline": [], "cursor": {}}, c);
        assert_eq!(ids(&aggregated), vec![1, 3, 2, 4]);
        // A sort decides the order; a filter keeps the scan order.
        let sorted = dispatch_full(&doc! {"find": "w", "sort": {"_id": -1}}, c);
        assert_eq!(ids(&sorted), vec![4, 3, 2, 1]);
        let filtered = dispatch_full(&doc! {"find": "w", "filter": {"v": {"$gte": 2}}}, c);
        assert_eq!(ids(&filtered), vec![3, 2, 4]);
        // There is no `{_id: 1}` index on the buckets to hint.
        let hinted = dispatch_full(&doc! {"find": "w", "hint": {"_id": 1}}, c);
        assert_eq!(hinted.get_i32("code").ok(), Some(2), "{hinted}");
    });
}

/// `system.buckets.<name>` read as empty and was not listed.
#[test]
fn the_buckets_can_be_read() {
    with_wt(|c| {
        seed(c);
        let count = dispatch(&doc! {"count": "system.buckets.w"}, c);
        assert_eq!(count.get_i32("n").ok(), Some(3), "{count}");
        let found = dispatch_full(&doc! {"find": "system.buckets.w"}, c);
        let buckets = batch(&found);
        let keys: Vec<String> = buckets[0].keys().cloned().collect();
        assert_eq!(keys, vec!["_id", "control", "meta", "data"]);
        let control = buckets[0].get_document("control").unwrap();
        assert_eq!(control.get_i32("version").ok(), Some(2));
        assert_eq!(control.get_i32("count").ok(), Some(2));
        assert_eq!(
            control.get_document("min").unwrap(),
            &doc! {"_id": 1, "t": at(0), "v": 1}
        );
        assert_eq!(
            control.get_document("max").unwrap(),
            &doc! {"_id": 3, "t": at(2), "v": 3}
        );
        let metas: Vec<&str> = buckets.iter().map(|b| b.get_str("meta").unwrap()).collect();
        assert_eq!(metas, vec!["a", "b", "a"]);
        // A filter, an aggregation and a view over them all see the same.
        let a = dispatch(
            &doc! {"count": "system.buckets.w", "query": {"meta": "a"}},
            c,
        );
        assert_eq!(a.get_i32("n").ok(), Some(2), "{a}");
        dispatch(
            &doc! {"create": "v", "viewOn": "system.buckets.w", "pipeline": []},
            c,
        );
        let through = dispatch(&doc! {"count": "v"}, c);
        assert_eq!(through.get_i32("n").ok(), Some(3), "{through}");
        let stats = dispatch_full(
            &doc! {"aggregate": "w", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}},
            c,
        );
        let stats = &batch(&stats)[0];
        assert_eq!(stats.get_str("ns").ok(), Some("t.system.buckets.w"));
        assert_eq!(stats.get_i32("count").ok(), Some(3));
        // The stage behind the read is not one a client may name.
        let direct = dispatch_full(
            &doc! {"aggregate": "w", "pipeline": [{"$_secantusBuckets": "w"}], "cursor": {}},
            c,
        );
        assert_eq!(direct.get_i32("code").ok(), Some(40324), "{direct}");
    });
}

/// The listings mongod gives: the bucket namespace, `system.views`, and each
/// index as it lies on the buckets.
#[test]
fn the_buckets_are_listed() {
    with_wt(|c| {
        seed(c);
        dispatch(&doc! {"create": "plain"}, c);
        let listed = dispatch_full(&doc! {"listCollections": 1}, c);
        let entries = batch(&listed);
        let names: Vec<&str> = entries.iter().map(|e| e.get_str("name").unwrap()).collect();
        assert_eq!(
            names,
            vec!["plain", "w", "system.buckets.w", "system.views"]
        );
        let buckets = &entries[2];
        assert_eq!(buckets.get_str("type").ok(), Some("collection"));
        let options = buckets.get_document("options").unwrap();
        assert_eq!(options.get_bool("clusteredIndex").ok(), Some(true));
        assert!(options.get_document("validator").is_ok());
        assert_eq!(
            options
                .get_document("timeseries")
                .unwrap()
                .get_str("timeField")
                .ok(),
            Some("t")
        );
        let views = dispatch_full(&doc! {"find": "system.views"}, c);
        assert_eq!(
            batch(&views),
            vec![
                doc! {"_id": "t.w", "viewOn": "system.buckets.w", "pipeline": [
                    {"$_internalUnpackBucket": {"timeField": "t", "metaField": "m",
                    "bucketMaxSpanSeconds": 3600}}
                ]}
            ]
        );
        let indexes = dispatch_full(&doc! {"listIndexes": "system.buckets.w"}, c);
        assert_eq!(
            batch(&indexes),
            vec![
                doc! {"v": 2, "key": {"meta": 1, "control.min.t": 1, "control.max.t": 1},
                "name": "m_1_t_1"}
            ]
        );
        // An index built on the buckets shows on the collection.
        let built = dispatch(
            &doc! {"createIndexes": "system.buckets.w", "indexes": [
                {"key": {"control.min.v": 1, "control.max.v": 1}, "name": "ix"}
            ]},
            c,
        );
        assert_eq!(built.get_i32("numIndexesAfter").ok(), Some(2), "{built}");
        let indexes = dispatch_full(&doc! {"listIndexes": "w"}, c);
        let shown = batch(&indexes);
        let ix = shown
            .iter()
            .find(|i| i.get_str("name") == Ok("ix"))
            .unwrap();
        assert_eq!(ix.get_document("key").unwrap(), &doc! {"v": 1});
    });
}

/// A document that is not a bucket is refused by the bucket schema, and
/// dropping the buckets drops the collection.
#[test]
fn the_buckets_take_no_document_by_hand() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch(
            &doc! {"insert": "system.buckets.w", "documents": [{"_id": 1}]},
            c,
        );
        let error = reply.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(error.get_i32("code").ok(), Some(121), "{reply}");
        let info = error.get_document("errInfo").unwrap();
        assert_eq!(info.get_i32("failingDocumentId").ok(), Some(1));
        assert_eq!(
            info.get_document("details")
                .unwrap()
                .get_str("operatorName")
                .ok(),
            Some("$jsonSchema")
        );
        let count = dispatch(&doc! {"count": "w"}, c);
        assert_eq!(count.get_i32("n").ok(), Some(4));
        let dropped = dispatch(&doc! {"drop": "system.buckets.w"}, c);
        assert_eq!(dropped, doc! {"nIndexesWas": 1, "ok": 1.0});
        let listed = dispatch_full(&doc! {"listCollections": 1}, c);
        assert!(batch(&listed).is_empty(), "{listed}");
    });
}

/// `validate` and `collStats` describe the buckets.
#[test]
fn validate_and_collstats_count_buckets() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch(&doc! {"validate": "w"}, c);
        assert_eq!(reply.get_str("ns").ok(), Some("t.system.buckets.w"));
        assert_eq!(reply.get_i32("nrecords").ok(), Some(3), "{reply}");
        assert_eq!(reply.get_i32("nIndexes").ok(), Some(1));
        assert_eq!(
            reply.get_document("keysPerIndex").unwrap(),
            &doc! {"m_1_t_1": 3}
        );
        assert_eq!(
            reply.get_document("indexDetails").unwrap(),
            &doc! {"m_1_t_1": {"valid": true, "spec": {"v": 2,
            "key": {"meta": 1, "control.min.t": 1, "control.max.t": 1}, "name": "m_1_t_1"}}}
        );
        let stats = dispatch(&doc! {"collStats": "w"}, c);
        let ts = stats.get_document("timeseries").unwrap();
        assert_eq!(ts.get_str("bucketsNs").ok(), Some("t.system.buckets.w"));
        assert_eq!(ts.get_i32("bucketCount").ok(), Some(3));
        assert_eq!(stats.get_i32("nindexes").ok(), Some(1));
        // A plain collection's `validate` names its own indexes.
        dispatch(&doc! {"insert": "p", "documents": [{"_id": 1, "a": 1}]}, c);
        let plain = dispatch(&doc! {"validate": "p"}, c);
        assert_eq!(plain.get_str("repairMode").ok(), Some("None"));
        assert_eq!(plain.get_i32("nrecords").ok(), Some(1));
        assert_eq!(
            plain.get_document("indexDetails").unwrap(),
            &doc! {"_id_": {"valid": true, "spec": {"v": 2, "key": {"_id": 1}, "name": "_id_"}}}
        );
    });
}

fn code(reply: &Document) -> Option<i32> {
    reply.get_i32("code").ok()
}

/// These four answered `no such command`.
#[test]
fn capped_copies_and_the_two_maintenance_commands() {
    with_wt(|c| {
        let docs: Vec<Bson> = (0..50)
            .map(|i| Bson::Document(doc! {"_id": i, "a": i, "pad": "x".repeat(100)}))
            .collect();
        dispatch(&doc! {"insert": "n", "documents": docs}, c);
        dispatch(
            &doc! {"createIndexes": "n", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            c,
        );
        dispatch(&doc! {"create": "v", "viewOn": "n", "pipeline": []}, c);
        // The size is checked before the collection is looked at.
        let no_size = dispatch(&doc! {"convertToCapped": "nope"}, c);
        assert_eq!(code(&no_size), Some(72), "{no_size}");
        let missing = dispatch(&doc! {"convertToCapped": "nope", "size": 4096}, c);
        assert_eq!(code(&missing), Some(26), "{missing}");
        let view = dispatch(&doc! {"convertToCapped": "v", "size": 4096}, c);
        assert_eq!(code(&view), Some(166), "{view}");
        // A copy: capped by size, the newest kept, only the `_id` index.
        let cloned = dispatch(
            &doc! {"cloneCollectionAsCapped": "n", "toCollection": "n2", "size": 2000},
            c,
        );
        assert_eq!(cloned, doc! {"ok": 1.0});
        let kept = dispatch_full(&doc! {"find": "n2"}, c);
        assert_eq!(ids(&kept), (35..50).collect::<Vec<i32>>());
        let again = dispatch(
            &doc! {"cloneCollectionAsCapped": "n", "toCollection": "n2", "size": 2000},
            c,
        );
        assert_eq!(code(&again), Some(48), "{again}");
        // In place: the collection is capped, and its secondary index is gone.
        let converted = dispatch(&doc! {"convertToCapped": "n", "size": 100000}, c);
        assert_eq!(converted, doc! {"ok": 1.0});
        let listed = dispatch_full(&doc! {"listCollections": 1, "filter": {"name": "n"}}, c);
        assert_eq!(
            batch(&listed)[0].get_document("options").unwrap(),
            &doc! {"capped": true, "size": 100000}
        );
        assert_eq!(
            dispatch(&doc! {"count": "n"}, c).get_i32("n").ok(),
            Some(50)
        );
        let indexes = dispatch_full(&doc! {"listIndexes": "n"}, c);
        assert_eq!(batch(&indexes).len(), 1);
        // No replica set name here, so the two answer as a standalone does.
        assert_eq!(
            dispatch(&doc! {"compact": "n"}, c),
            doc! {"bytesFreed": 0, "ok": 1.0}
        );
        assert_eq!(code(&dispatch(&doc! {"compact": "nope"}, c)), Some(26));
        assert_eq!(code(&dispatch(&doc! {"compact": "v"}, c)), Some(166));
        assert_eq!(
            dispatch(&doc! {"reIndex": "n"}, c),
            doc! {"nIndexesWas": 1, "nIndexes": 1,
            "indexes": [{"v": 2, "key": {"_id": 1}, "name": "_id_"}], "ok": 1.0}
        );
        // On a replica-set member mongod refuses both.
        c.replica_set_name = Some("rs0".into());
        assert_eq!(code(&dispatch(&doc! {"compact": "n"}, c)), Some(20));
        assert_eq!(code(&dispatch(&doc! {"reIndex": "n"}, c)), Some(20));
        let forced = dispatch(&doc! {"compact": "n", "force": true}, c);
        assert_eq!(forced.get_i32("bytesFreed").ok(), Some(0), "{forced}");
    });
}
