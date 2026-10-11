//! A collection's default collation and its `_id`, and what `$out` and
//! `$merge` do when the target refuses a document. Every expectation is what
//! mongod 8.2.11 answered to the same commands.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_admin, dispatch_full, with_transactions, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn collated(c: &mut CommandContext, name: &str) {
    let reply = dispatch(
        &doc! {"create": name, "collation": {"locale": "en", "strength": 2}},
        c,
    );
    assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
}

fn insert(c: &mut CommandContext, name: &str, ids: &[&str]) -> Document {
    let docs: Vec<Bson> = ids
        .iter()
        .map(|id| Bson::Document(doc! {"_id": *id}))
        .collect();
    dispatch(
        &doc! {"insert": name, "documents": docs, "ordered": false},
        c,
    )
}

fn refused(reply: &Document) -> Vec<i32> {
    reply
        .get_array("writeErrors")
        .map(|errors| {
            errors
                .iter()
                .map(|e| e.as_document().unwrap().get_i32("index").unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn ids(c: &mut CommandContext, name: &str) -> Vec<String> {
    let reply = dispatch_full(
        &doc! {"find": name, "sort": {"_id": 1}, "collation": {"locale": "simple"}},
        c,
    );
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().get_str("_id").unwrap().to_string())
        .collect()
}

/// `"A"` went in beside `"a"`: the `_id` index is keyed by value.
#[test]
fn an_id_is_unique_by_the_default_collation() {
    with_wt(|c| {
        collated(c, "c");
        assert!(refused(&insert(c, "c", &["a"])).is_empty());
        let reply = insert(c, "c", &["A", "b", "B", "c"]);
        assert_eq!(refused(&reply), vec![0, 2], "{reply}");
        let error = reply.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(error.get_i32("code").ok(), Some(11000));
        let errmsg = error.get_str("errmsg").unwrap();
        assert!(
            errmsg.contains("index: _id_ collation: { locale: \"en\""),
            "{errmsg}"
        );
        assert!(
            errmsg.contains("dup key: { _id: \"CollationKey(0x"),
            "{errmsg}"
        );
        assert_eq!(
            error.get_array("hexEncoded").ok(),
            Some(&vec![Bson::Boolean(true)])
        );
        assert_eq!(ids(c, "c"), vec!["a", "b", "c"]);
        // An upsert that matches nothing by a simple comparison still may not
        // insert a second spelling.
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": "C"}, "u": {"$set": {"x": 1}},
            "upsert": true, "collation": {"locale": "simple"}}]},
            c,
        );
        assert_eq!(refused(&reply), vec![0], "{reply}");
    });
}

/// The claim is released with the document and follows a rename.
#[test]
fn the_claim_follows_delete_rename_and_drop() {
    with_wt(|c| {
        collated(c, "c");
        insert(c, "c", &["a", "b"]);
        dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"_id": "a"}, "limit": 1}]},
            c,
        );
        assert!(refused(&insert(c, "c", &["A"])).is_empty());
        assert_eq!(refused(&insert(c, "c", &["a"])), vec![0]);
        let reply = dispatch_admin(&doc! {"renameCollection": "t.c", "to": "t.d"}, c);
        assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
        assert_eq!(refused(&insert(c, "d", &["a", "B", "q"])), vec![0, 1]);
        // The old name is free again, and has no collation.
        assert!(refused(&insert(c, "c", &["A", "a"])).is_empty());
        dispatch(&doc! {"drop": "d"}, c);
        collated(c, "d");
        assert_eq!(refused(&insert(c, "d", &["A", "a"])), vec![1]);
    });
}

/// A transaction that aborts gives its claim back.
#[test]
fn an_aborted_transaction_releases_its_claim() {
    with_wt(|c| {
        with_transactions(c);
        collated(c, "c");
        let lsid = doc! {"id": bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![7; 16]
        }};
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": "t"}], "lsid": lsid.clone(),
            "txnNumber": 1_i64, "autocommit": false, "startTransaction": true},
            c,
        );
        assert!(refused(&reply).is_empty(), "{reply}");
        // The transaction's own second spelling is refused inside it.
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": "T"}], "lsid": lsid.clone(),
            "txnNumber": 1_i64, "autocommit": false},
            c,
        );
        assert_eq!(refused(&reply), vec![0], "{reply}");
        let reply = dispatch_admin(
            &doc! {"abortTransaction": 1, "lsid": lsid, "txnNumber": 1_i64, "autocommit": false},
            c,
        );
        assert!(reply.get_f64("ok").is_ok(), "{reply}");
        assert!(refused(&insert(c, "c", &["T"])).is_empty());
        assert_eq!(ids(c, "c"), vec!["T"]);
    });
}

/// A clustered collection's duplicate names no index and adds what it found.
#[test]
fn a_clustered_duplicate_names_no_index() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "p", "clusteredIndex": {"key": {"_id": 1}, "unique": true}},
            c,
        );
        let reply = dispatch(
            &doc! {"insert": "p", "documents": [{"_id": 5}, {"_id": 5}]},
            c,
        );
        let error = reply.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(
            error.get_str("errmsg").ok(),
            Some("E11000 duplicate key error collection: t.p dup key: { _id: 5 } found value: {}")
        );
        assert_eq!(
            error.get_document("foundValue").ok(),
            Some(&Document::new())
        );
    });
}

fn aggregate(c: &mut CommandContext, pipeline: Vec<Document>) -> Document {
    dispatch_full(
        &doc! {"aggregate": "src", "pipeline": pipeline, "cursor": {}},
        c,
    )
}

/// `$merge` matched by a simple comparison, so `"M"` was inserted beside
/// `"m"`. It matches under the target's collation and may not change `_id`.
#[test]
fn merge_matches_under_the_targets_collation() {
    with_wt(|c| {
        for (mode, code) in [
            (Bson::String("merge".into()), Some(66)),
            (Bson::String("replace".into()), Some(66)),
            (Bson::String("keepExisting".into()), None),
            (
                Bson::Array(vec![Bson::Document(doc! {"$set": {"z": 1}})]),
                None,
            ),
        ] {
            dispatch(&doc! {"drop": "a"}, c);
            dispatch(&doc! {"drop": "src"}, c);
            collated(c, "a");
            dispatch(
                &doc! {"insert": "src", "documents": [{"_id": "m", "v": 1}, {"_id": "M", "v": 2}]},
                c,
            );
            let reply = aggregate(c, vec![doc! {"$merge": {"into": "a", "whenMatched": mode}}]);
            assert_eq!(reply.get_i32("code").ok(), code, "{reply}");
            if code.is_some() {
                assert!(
                    reply.get_str("errmsg").unwrap().starts_with(
                        "Executor error during aggregate command on namespace: t.src :: caused \
                         by :: $merge failed to update the matching document"
                    ),
                    "{reply}"
                );
            }
            assert_eq!(ids(c, "a"), vec!["m"]);
        }
    });
}

/// A `$out` whose output the target refuses left the target emptied or
/// half-written. It is built beside the target and swapped in.
#[test]
fn a_failed_out_leaves_the_target_alone() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "src", "documents": [{"_id": 1, "k": 0}, {"_id": 2, "k": 0}]},
            c,
        );
        dispatch(
            &doc! {"createIndexes": "t3", "indexes": [{"key": {"k": 1}, "name": "k_1", "unique": true}]},
            c,
        );
        dispatch(
            &doc! {"insert": "t3", "documents": [{"_id": 99, "k": 5}]},
            c,
        );
        let reply = aggregate(c, vec![doc! {"$out": "t3"}]);
        assert_eq!(reply.get_i32("code").ok(), Some(11000), "{reply}");
        let errmsg = reply.get_str("errmsg").unwrap();
        assert!(
            errmsg.starts_with(
                "Executor error during aggregate command on namespace: t.src :: caused by :: \
                 E11000 duplicate key error collection: t.tmp.agg_out."
            ),
            "{errmsg}"
        );
        let found = dispatch_full(&doc! {"find": "t3"}, c);
        let batch = found
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(batch, &vec![Bson::Document(doc! {"_id": 99, "k": 5})]);
        // Nothing is left behind, and the index is still there.
        let listed = dispatch_full(&doc! {"listCollections": 1, "nameOnly": true}, c);
        let names: Vec<&str> = listed
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|d| d.as_document().unwrap().get_str("name").unwrap())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with("tmp.agg_out")),
            "{names:?}"
        );
        // A duplicate `_id` in the output was dropped without a word.
        let reply = aggregate(
            c,
            vec![doc! {"$project": {"_id": "$k"}}, doc! {"$out": "t4"}],
        );
        assert_eq!(reply.get_i32("code").ok(), Some(11000), "{reply}");
        // A capped target is refused.
        dispatch(&doc! {"create": "cap", "capped": true, "size": 4096}, c);
        let reply = aggregate(c, vec![doc! {"$out": "cap"}]);
        assert_eq!(reply.get_i32("code").ok(), Some(17152), "{reply}");
        // And a `$out` that works still replaces the documents and keeps the index.
        dispatch(
            &doc! {"delete": "src", "deletes": [{"q": {"_id": 2}, "limit": 1}]},
            c,
        );
        let reply = aggregate(c, vec![doc! {"$out": "t3"}]);
        assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
        let found = dispatch_full(&doc! {"find": "t3"}, c);
        let batch = found
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(batch, &vec![Bson::Document(doc! {"_id": 1, "k": 0})]);
        let indexes = dispatch_full(&doc! {"listIndexes": "t3"}, c);
        assert_eq!(
            indexes
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .len(),
            2
        );
    });
}
