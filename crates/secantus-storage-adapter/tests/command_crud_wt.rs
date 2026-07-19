//! Real-WiredTiger ports of the `insert` / `update` / `delete` / `count`
//! command unit tests. State is set up and verified entirely through `dispatch`
//! over a real `WtStorage` — validators come from real `create` commands and
//! results are checked with real reads.

mod common;

use bson::{doc, Bson, Document};
use common::with_wt;
use secantus_commands::{dispatch, CommandContext};

fn count(c: &mut CommandContext) -> i32 {
    dispatch(&doc! {"count": "c"}, c).get_i32("n").unwrap()
}

/// A `find` reply's cursor batch as `Bson` docs. A no-projection `find` now
/// hands its batch to the server as pre-encoded blobs via `ctx.pending_batch`
/// (the raw-BSON reply fast path) rather than a `firstBatch` array in the reply
/// document; read whichever the handler produced.
fn fb(reply: &Document, c: &CommandContext) -> Vec<Bson> {
    match &c.pending_batch {
        Some(pb) => pb
            .batch
            .iter()
            .map(|b| Bson::Document(Document::from_reader(&mut &b[..]).unwrap()))
            .collect(),
        None => reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .clone(),
    }
}

#[test]
fn capped_eviction_is_fifo_with_non_monotonic_ids() {
    // A capped collection evicts in true insertion order (FIFO), even when the
    // _ids are non-monotonic — the first-inserted doc is evicted first, not the
    // lowest _id. Regression for the natural-order eviction fix.
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "capped": true, "max": 2i64, "size": 100000i64},
            c,
        );
        // Insert in DECREASING _id order: 5, then 3, then 1.
        for id in [5, 3, 1] {
            dispatch(&doc! {"insert": "c", "documents": [{"_id": id}]}, c);
        }
        // max=2: after inserting 1 (the 3rd), the oldest (_id 5, first inserted)
        // is evicted — FIFO. id_key order would have wrongly evicted _id 1.
        let reply = dispatch(&doc! {"find": "c", "sort": {"_id": 1}}, c);
        let ids: Vec<i32> = fb(&reply, c)
            .iter()
            .map(|b| b.as_document().unwrap().get_i32("_id").unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![1, 3],
            "FIFO should keep the two most-recent inserts"
        );
    });
}

#[test]
fn insert_then_count() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get("writeErrors").is_none());
        assert_eq!(count(c), 2);
    });
}

#[test]
fn insert_rejects_validator_violation() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"a": {"$exists": true}}},
            c,
        );
        // doc 0 violates (no `a`), doc 1 passes.
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2, "a": 1}],
            "ordered": false},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1, "only the valid doc inserts");
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("code").unwrap(), 121);
        assert_eq!(e.get_i32("index").unwrap(), 0);
        // mongod attaches errInfo (failingDocumentId + per-operator details).
        let info = e.get_document("errInfo").unwrap();
        assert_eq!(info.get_i32("failingDocumentId").unwrap(), 1);
        let details = info.get_document("details").unwrap();
        assert_eq!(details.get_str("operatorName").unwrap(), "$exists");
        assert_eq!(details.get_str("reason").unwrap(), "field was missing");
    });
}

#[test]
fn insert_validation_errinfo_details_carries_considered_value() {
    // A present-but-wrong field reports the full per-operator details mongod
    // synthesises — including consideredValue/consideredType — which
    // mongo-csharp-driver `WriteError_details` and mongo-java-driver
    // `findOneAndUpdate-errorResponse` assert.
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"x": {"$type": "string"}}},
            c,
        );
        let r = dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]}, c);
        let e = r.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(e.get_i32("code").unwrap(), 121);
        let details = e
            .get_document("errInfo")
            .unwrap()
            .get_document("details")
            .unwrap();
        assert_eq!(details.get_str("operatorName").unwrap(), "$type");
        assert_eq!(
            details.get_document("specifiedAs").unwrap(),
            &doc! {"x": {"$type": "string"}}
        );
        assert_eq!(details.get_str("reason").unwrap(), "type did not match");
        assert_eq!(details.get("consideredValue"), Some(&Bson::Int32(1)));
        assert_eq!(details.get_str("consideredType").unwrap(), "int");
    });
}

#[test]
fn duplicate_key_writeerror_message_shape() {
    // E11000 writeError carries mongod's exact errmsg + keyPattern/keyValue
    // (mongo-php-driver writeError-getMessage / writeResult-getWriteErrors).
    with_wt(|c| {
        let r = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 1}], "ordered": true},
            c,
        );
        let e = r.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(e.get_i32("code").unwrap(), 11000);
        assert_eq!(e.get_i32("index").unwrap(), 1);
        assert_eq!(
            e.get_str("errmsg").unwrap(),
            "E11000 duplicate key error collection: t.c index: _id_ dup key: { _id: 1 }"
        );
        assert_eq!(e.get_document("keyPattern").unwrap(), &doc! {"_id": 1});
        assert_eq!(e.get_document("keyValue").unwrap(), &doc! {"_id": 1});
    });
}

#[test]
fn upsert_with_code_id_succeeds() {
    // A bson Code value is a valid _id (pymongo ranks it as a string); the upsert
    // inserts and reports it (mongo-php-driver writeResult-getUpsertedIds).
    with_wt(|c| {
        let code = Bson::JavaScriptCode("function(){}".into());
        let r = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {"_id": code.clone()}, "u": {"$set": {"x": 1}}, "upsert": true}
            ]},
            c,
        );
        assert_eq!(r.get_i32("n").unwrap(), 1);
        let up = r.get_array("upserted").unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].as_document().unwrap().get("_id"), Some(&code));
    });
}

#[test]
fn insert_bypass_document_validation_skips_validator() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"a": {"$exists": true}}},
            c,
        );
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}],
            "bypassDocumentValidation": true},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        assert!(reply.get("writeErrors").is_none());
    });
}

#[test]
fn insert_empty_documents_is_invalid_length() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"insert": "c", "documents": []}, c);
        assert_eq!(reply.get_i32("code").unwrap(), 4);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidLength");
    });
}

#[test]
fn insert_id_with_dollar_prefix_rejected() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": {"$bad": 1}}]},
            c,
        );
        // ordered (default) + a pre-check failure ⇒ nothing inserted, one error.
        assert_eq!(reply.get_i32("n").unwrap(), 0);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("code").unwrap(), 2);
        assert!(e.get_str("errmsg").unwrap().contains("$bad"));
    });
}

#[test]
fn insert_duplicate_key_unordered_continues_and_remaps_index() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 2}]}, c);
        // unordered batch: [ok(1), dup(2), ok(3)] ⇒ n=2, one writeError at index 1.
        let reply = dispatch(
            &doc! {
                "insert": "c",
                "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}],
                "ordered": false,
            },
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("index").unwrap(), 1, "index remapped to original");
        assert_eq!(e.get_i32("code").unwrap(), 11000);
    });
}

#[test]
fn insert_pre_error_index_remap_unordered() {
    // [bad-$id(0), ok(1), dup(2)] unordered: the storage error on the dup
    // (original index 2) must remap correctly past the pre-error at 0.
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 9}]}, c);
        let reply = dispatch(
            &doc! {
                "insert": "c",
                "documents": [{"_id": {"$x": 1}}, {"_id": 5}, {"_id": 9}],
                "ordered": false,
            },
            c,
        );
        // _id 5 inserted; _id 9 duplicate; _id {$x} pre-rejected.
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 2);
        let pre = we[0].as_document().unwrap();
        assert_eq!(pre.get_i32("index").unwrap(), 0);
        assert_eq!(pre.get_i32("code").unwrap(), 2);
        let dup = we[1].as_document().unwrap();
        assert_eq!(dup.get_i32("index").unwrap(), 2);
        assert_eq!(dup.get_i32("code").unwrap(), 11000);
    });
}

#[test]
fn delete_removes_and_counts() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}, {"_id": 3, "x": 2}]},
            c,
        );
        // limit 0 ⇒ delete all matching x:1
        let reply = dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 0}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(count(c), 1);
    });
}

#[test]
fn delete_limit_one() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            c,
        );
        let reply = dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 1}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
    });
}

#[test]
fn count_skip_and_limit_clamp() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}]},
            c,
        );
        assert_eq!(
            dispatch(&doc! {"count": "c", "skip": 1}, c)
                .get_i32("n")
                .unwrap(),
            3
        );
        assert_eq!(
            dispatch(&doc! {"count": "c", "limit": 2}, c)
                .get_i32("n")
                .unwrap(),
            2
        );
    });
}

#[test]
fn count_hint_honours_sparse_index() {
    // count + a sparse-index hint counts only the docs present in that index
    // (php-lib Count testHintOption); a non-sparse / _id hint counts all docs.
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"x": 1}, {"x": 2}, {"y": 3}]},
            c,
        );
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"x": 1}, "sparse": true, "name": "sparse_x"},
                {"key": {"y": 1}, "name": "y_1"},
            ]},
            c,
        );
        // Sparse index on x → only the 2 docs with x.
        for hint in [
            Bson::Document(doc! {"x": 1}),
            Bson::String("sparse_x".into()),
        ] {
            let r = dispatch(&doc! {"count": "c", "hint": hint.clone()}, c);
            assert_eq!(r.get_i32("n").unwrap(), 2, "sparse hint {hint:?}");
        }
        // Non-sparse y index and _id → all 3 docs (missing-field entries present).
        for hint in [Bson::String("y_1".into()), Bson::String("_id_".into())] {
            let r = dispatch(&doc! {"count": "c", "hint": hint.clone()}, c);
            assert_eq!(r.get_i32("n").unwrap(), 3, "non-sparse hint {hint:?}");
        }
    });
}

#[test]
fn find_returns_insertion_order_for_mixed_id_types() {
    // Unsorted find returns insertion order, not _id-sort order — the case
    // php-lib BulkWriteFunctionalTest::testInserts pins (mixed _id types inserted
    // out of _id order).
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"_id": 1, "x": 11}, {"x": 22}, {"_id": "foo", "x": 33}, {"_id": "bar", "x": 44}
            ]},
            c,
        );
        let found = dispatch(&doc! {"find": "c"}, c);
        let xs: Vec<i32> = fb(&found, c)
            .iter()
            .map(|b| b.as_document().unwrap().get_i32("x").unwrap())
            .collect();
        assert_eq!(
            xs,
            vec![11, 22, 33, 44],
            "insertion order regardless of _id type"
        );
    });
}

#[test]
fn find_on_symbol_and_code_values() {
    // Equality queries on Symbol / JS-Code (with scope) values — mongo-node-driver
    // "handles BSON type inserts".
    with_wt(|c| {
        let code = Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
            code: "function () {}".into(),
            scope: doc! {"a": 55},
        });
        dispatch(
            &doc! {"insert": "c", "documents": [{
                "_id": 1,
                "symbol": Bson::Symbol("abcdefghijkl".into()),
                "code": code.clone(),
                "minkey": Bson::MinKey,
                "maxkey": Bson::MaxKey,
            }]},
            c,
        );
        for filter in [
            doc! {"symbol": Bson::Symbol("abcdefghijkl".into())},
            doc! {"code": code.clone()},
            doc! {"minkey": Bson::MinKey},
            doc! {"maxkey": Bson::MaxKey},
        ] {
            let r = dispatch(&doc! {"find": "c", "filter": filter.clone()}, c);
            assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{filter:?}");
            assert_eq!(fb(&r, c).len(), 1, "{filter:?}");
        }
    });
}

#[test]
fn update_set_code_value() {
    // Insert + $set a JS-Code value — mongo-node-driver "function serialization".
    with_wt(|c| {
        let f1 = Bson::JavaScriptCode("function (x){return x;}".into());
        let f2 = Bson::JavaScriptCode("function (y){return y;}".into());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "a": 1, "f": f1}]},
            c,
        );
        let r = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"a": 1}, "u": {"$set": {"f": f2.clone()}}}]},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "update reply: {r:?}");
        assert_eq!(r.get_i32("nModified").unwrap(), 1);
        let found = dispatch(&doc! {"find": "c"}, c);
        let f = fb(&found, c)[0].as_document().unwrap().get("f").cloned();
        assert_eq!(f, Some(f2));
    });
}

#[test]
fn data_command_without_storage_is_internal_error() {
    let mut c = CommandContext::new(1); // no storage attached
    let reply = dispatch(&doc! {"count": "c"}, &mut c);
    assert_eq!(reply.get_i32("code").unwrap(), 1);
    assert_eq!(reply.get_str("codeName").unwrap(), "InternalError");
}

#[test]
fn update_set_modifies_and_counts() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]}, c);
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"x": 2}}}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        assert_eq!(reply.get_i32("nModified").unwrap(), 1);
        assert!(reply.get("upserted").is_none());
        assert!(reply.get("writeErrors").is_none());
    });
}

#[test]
fn update_bit_operator() {
    // $bit and/or/xor on an integer field (mongo-node-driver "apply bit operator").
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "b": 5}]}, c);
        let r = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$bit": {"b": {"and": 1}}}}]},
            c,
        );
        assert_eq!(r.get_i32("nModified").unwrap(), 1);
        let found = dispatch(&doc! {"find": "c"}, c);
        let b = fb(&found, c)[0]
            .as_document()
            .unwrap()
            .get_i32("b")
            .unwrap();
        assert_eq!(b, 1, "5 & 1 == 1");
    });
}

#[test]
fn update_multi_touches_all_matches() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            c,
        );
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"x": 1}, "u": {"$set": {"y": 9}}, "multi": true}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_i32("nModified").unwrap(), 2);
    });
}

#[test]
fn update_upsert_reports_upserted_id() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 5}, "u": {"$set": {"a": 1}}, "upsert": true}]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1, "upsert counts toward n");
        assert_eq!(reply.get_i32("nModified").unwrap(), 0);
        let up = reply.get_array("upserted").unwrap();
        assert_eq!(up.len(), 1);
        let e = up[0].as_document().unwrap();
        assert_eq!(e.get_i32("index").unwrap(), 0);
        assert_eq!(e.get_i32("_id").unwrap(), 5);
    });
}

#[test]
fn update_sort_option_rejected_pre_8() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "sort": {"a": 1}}]},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
    });
}

#[test]
fn update_pipeline_unknown_stage_is_command_error() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {}, "u": [{"$badStage": {}}]}]},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 168);
        assert_eq!(
            reply.get_str("codeName").unwrap(),
            "InvalidPipelineOperator"
        );
    });
}

#[test]
fn update_valid_pipeline_applies_via_storage() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "a": 0}, {"_id": 2, "a": 0}]},
            c,
        );
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {}, "u": [{"$set": {"a": 1}}], "multi": true}
            ]},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get("writeErrors").is_none(), "pipeline now applies");
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_i32("nModified").unwrap(), 2);
        // Verify via a real read: every doc now has a == 1.
        let found = dispatch(&doc! {"find": "c"}, c);
        for b in fb(&found, c) {
            assert_eq!(b.as_document().unwrap().get("a"), Some(&Bson::Int32(1)));
        }
    });
}
