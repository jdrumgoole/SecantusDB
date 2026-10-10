//! Real-WiredTiger ports of the `insert` / `update` / `delete` / `count`
//! command unit tests. State is set up and verified entirely through `dispatch`
//! over a real `WtStorage` — validators come from real `create` commands and
//! results are checked with real reads.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
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

/// The `_id`s left in capped collection `c`, in natural order.
fn capped_ids(c: &mut CommandContext) -> Vec<i32> {
    let reply = dispatch(&doc! {"find": "c", "batchSize": 100_000_i32}, c);
    fb(&reply, c)
        .iter()
        .map(|b| b.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

fn capped_batch(ids: std::ops::Range<i32>, payload: usize) -> Vec<Bson> {
    ids.map(|i| Bson::Document(doc! {"_id": i, "p": "x".repeat(payload)}))
        .collect()
}

/// Every expectation here is what mongod 8.2.11 returned for the same
/// commands (2026-10-09). The batch that overflows a capped collection is
/// evicted from like any other documents; until then it was spared, so one
/// `insert_many` could take the collection past its bounds without limit.
#[test]
fn capped_bounds_hold_within_one_insert_batch() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "capped": true, "max": 3i64, "size": 1048576i64},
            c,
        );
        let reply = dispatch(&doc! {"insert": "c", "documents": capped_batch(0..5, 1)}, c);
        assert_eq!(reply.get_i32("n").unwrap(), 5);
        assert_eq!(capped_ids(c), vec![2, 3, 4]);
        // A duplicate in an unordered batch is reported and the rest land.
        let reply = dispatch(
            &doc! {"insert": "c", "ordered": false,
            "documents": [{"_id": 7}, {"_id": 7}, {"_id": 8}, {"_id": 9}, {"_id": 10}]},
            c,
        );
        assert_eq!(reply.get_array("writeErrors").unwrap().len(), 1);
        assert_eq!(capped_ids(c), vec![8, 9, 10]);
    });
    // Larger than one internal insert chunk (1,000 documents).
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "capped": true, "max": 1500i64, "size": 16777216i64},
            c,
        );
        dispatch(
            &doc! {"insert": "c", "documents": capped_batch(0..2500, 1)},
            c,
        );
        let ids = capped_ids(c);
        assert_eq!((ids.len(), ids[0], ids[ids.len() - 1]), (1500, 1000, 2499));
    });
    // Bounded by size alone.
    with_wt(|c| {
        dispatch(&doc! {"create": "c", "capped": true, "size": 4096i64}, c);
        dispatch(
            &doc! {"insert": "c", "documents": capped_batch(0..40, 500)},
            c,
        );
        assert_eq!(capped_ids(c), (33..40).collect::<Vec<_>>());
    });
}

/// The newest document stays even when it alone is larger than `size`
/// (mongod 8.2.11: sixty 3,000-byte inserts into `size: 1000` leave `[59]`).
#[test]
fn capped_collection_keeps_its_newest_document_whatever_its_size() {
    for one_batch in [true, false] {
        with_wt(|c| {
            dispatch(&doc! {"create": "c", "capped": true, "size": 1000i64}, c);
            if one_batch {
                dispatch(
                    &doc! {"insert": "c", "documents": capped_batch(0..60, 3000)},
                    c,
                );
            } else {
                for i in 0..60 {
                    dispatch(
                        &doc! {"insert": "c", "documents": capped_batch(i..i + 1, 3000)},
                        c,
                    );
                }
            }
            assert_eq!(capped_ids(c), vec![59], "one_batch={one_batch}");
        });
    }
}

/// An upsert's insert is held to the cap like any other insert (mongod
/// 8.2.11). It used to be skipped, so upserts grew a capped collection
/// without limit.
#[test]
fn capped_bounds_hold_for_upserts() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "capped": true, "max": 3i64, "size": 100000i64},
            c,
        );
        dispatch(&doc! {"insert": "c", "documents": capped_batch(0..3, 1)}, c);
        for id in [10, 11, 12] {
            let reply = dispatch(
                &doc! {"update": "c", "updates": [{"q": {"_id": id}, "u": {"$set": {"a": 1}}, "upsert": true}]},
                c,
            );
            assert_eq!(reply.get_array("upserted").unwrap().len(), 1, "{reply}");
        }
        assert_eq!(capped_ids(c), vec![10, 11, 12]);
        dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 77}, "update": {"$set": {"a": 1}}, "upsert": true},
            c,
        );
        assert_eq!(capped_ids(c), vec![11, 12, 77]);
        // A replacement upsert, and four upserts in one command.
        dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 88}, "u": {"a": 2}, "upsert": true}]},
            c,
        );
        assert_eq!(capped_ids(c), vec![12, 77, 88]);
        let many: Vec<Bson> = (90..94)
            .map(|i| {
                Bson::Document(doc! {"q": {"_id": i}, "u": {"$set": {"a": 1}}, "upsert": true})
            })
            .collect();
        dispatch(&doc! {"update": "c", "updates": many}, c);
        assert_eq!(capped_ids(c), vec![91, 92, 93]);
    });
}

/// `max` of zero or less is "no document limit" on mongod, which keeps all
/// five. Read as a limit, it evicted everything except the newest document.
#[test]
fn capped_max_of_zero_or_less_is_no_limit() {
    for max in [0i64, -5] {
        with_wt(|c| {
            dispatch(
                &doc! {"create": "c", "capped": true, "max": max, "size": 100000i64},
                c,
            );
            dispatch(&doc! {"insert": "c", "documents": capped_batch(0..5, 1)}, c);
            assert_eq!(capped_ids(c), vec![0, 1, 2, 3, 4], "max={max}");
        });
    }
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
        // mongod 8.2.11's wording for a failed `$exists: true` (2026-09-30).
        assert_eq!(details.get_str("reason").unwrap(), "path does not exist");
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
        // `InvalidLength` is code **16**; this asserted 4, which is `NoSuchKey`.
        // Probed on mongod 8.2.11 — and `bulkWrite` in this same codebase
        // already answered 16. `update` / `delete` share the rule now too.
        for cmd in [
            doc! {"insert": "c", "documents": []},
            doc! {"update": "c", "updates": []},
            doc! {"delete": "c", "deletes": []},
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), 16, "{cmd:?}");
            assert_eq!(reply.get_str("codeName").unwrap(), "InvalidLength");
        }
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
        // 52 `DollarPrefixedFieldName` on mongod 8.2.11 (2026-10-10); this
        // asserted the 2 the server used to answer.
        assert_eq!(e.get_i32("code").unwrap(), 52);
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
        assert_eq!(pre.get_i32("code").unwrap(), 52);
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

/// MongoDB 8.0's `sort` on an update statement: match in sort order, update the
/// FIRST one. This test used to assert the option was REJECTED, which was right
/// while the server advertised 7.0 and wrong the moment it advertised 8.2.11 --
/// the driver `*-sort` specs assert both directions, gated on the version.
#[test]
fn update_sort_updates_the_first_in_sort_order() {
    with_wt(|c| {
        for (id, v) in [(1, 3), (2, 1), (3, 2)] {
            dispatch(&doc! {"insert": "c", "documents": [{"_id": id, "v": v}]}, c);
        }
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {}, "u": {"$set": {"hit": 1}}, "sort": {"v": 1}}
            ]},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        // `v: 1` sorts _id 2 first, so that is the document that was updated.
        let found = dispatch(&doc! {"find": "c", "filter": {"hit": 1}}, c);
        let batch = fb(&found, c);
        assert_eq!(batch.len(), 1);
        assert_eq!(
            batch[0].as_document().unwrap().get_i32("_id").unwrap(),
            2,
            "sort asc should pick the lowest v"
        );
    });
}

/// mongod refuses the combination -- probed 8.2.11.
#[test]
fn update_sort_with_multi_is_rejected() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {}, "u": {"$set": {"a": 1}}, "multi": true, "sort": {"a": 1}}
            ]},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
        assert_eq!(
            reply.get_str("errmsg").unwrap(),
            "Cannot specify sort with multi=true"
        );
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

/// `bulkWrite` takes `bypassEmptyTsReplacement`, and still refuses a real
/// unknown field.
///
/// mongod 8.2.11 accepts this on `bulkWrite`, `insert` AND `update` (probed
/// 2026-09-18). Both servers already took it on `insert` / `update`; only
/// `bulkWrite` refused, which failed all 17 of the Go driver's
/// `TestClient_BulkWrite_AddCommandFields` cases -- the driver appends the
/// field by default on 8.x, so this was a refusal of an ordinary command.
///
/// The second half matters as much as the first: the fix widened the
/// known-field list, and must not have disabled the gate that rejects a
/// genuinely unknown field.
#[test]
fn bulk_write_accepts_bypass_empty_ts_replacement() {
    with_wt(|c| {
        c.db_name = "admin".into();
        for value in [true, false] {
            let reply = dispatch(
                &doc! {
                    "bulkWrite": 1,
                    "nsInfo": [{"ns": "t.c"}],
                    "ops": [{"insert": 0, "document": {}}],
                    "bypassEmptyTsReplacement": value,
                },
                c,
            );
            assert_eq!(
                reply.get_f64("ok").unwrap(),
                1.0,
                "value={value} -> {reply:?}"
            );
        }

        let reply = dispatch(
            &doc! {
                "bulkWrite": 1,
                "nsInfo": [{"ns": "t.c"}],
                "ops": [{"insert": 0, "document": {}}],
                "totallyBogusField": true,
            },
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 40415);
    });
}

/// A cursor id is an int64 on the wire, and the TYPE is the whole assertion.
///
/// The Python server sent a bare `0` here, which BSON encodes as a 32-bit
/// integer; the Go driver type-checks it and refused. Rust's `0i64` is already
/// right, so this test is a TRIPWIRE rather than a fix -- `doc! {"id": 0}`
/// would compile perfectly well and silently reintroduce the bug.
#[test]
fn bulk_write_reply_cursor_id_is_int64() {
    with_wt(|c| {
        c.db_name = "admin".into();
        let reply = dispatch(
            &doc! {
                "bulkWrite": 1,
                "nsInfo": [{"ns": "t.c"}],
                "ops": [{"insert": 0, "document": {"_id": 1}}],
            },
            c,
        );
        let id = reply.get_document("cursor").unwrap().get("id").unwrap();
        assert!(
            matches!(id, Bson::Int64(0)),
            "cursor.id must be Int64, got {id:?}"
        );
    });
}

/// `bulkWrite`'s results are a real cursor when they do not fit one batch.
///
/// Measured against mongod 8.2.11 (2026-09-18). The boundary is strictly "more
/// remain" -- an exact fit keeps NO cursor, which is where this differs from
/// `find` (hence `bounded: true` at the call site).
#[test]
fn bulk_write_results_page_through_a_cursor() {
    with_wt(|c| {
        c.db_name = "admin".into();
        let bulk = |c: &mut CommandContext, n: i32, extra: Document| {
            let ops: Vec<Bson> = (0..n)
                .map(|i| Bson::Document(doc! {"insert": 0, "document": {"i": i}}))
                .collect();
            let mut cmd = doc! {"bulkWrite": 1, "nsInfo": [{"ns": "t.c"}], "ops": ops};
            for (k, v) in extra {
                cmd.insert(k, v);
            }
            dispatch(&cmd, c)
        };

        // (label, ops, extra, cursor expected, firstBatch len)
        let cases: Vec<(&str, i32, Document, bool, usize)> = vec![
            ("no cursor option", 5, doc! {}, false, 5),
            (
                "batchSize under count",
                5,
                doc! {"cursor": {"batchSize": 2}},
                true,
                2,
            ),
            ("batchSize 0", 5, doc! {"cursor": {"batchSize": 0}}, true, 0),
            ("exact fit", 2, doc! {"cursor": {"batchSize": 2}}, false, 2),
            (
                "room to spare",
                2,
                doc! {"cursor": {"batchSize": 5}},
                false,
                2,
            ),
            ("errorsOnly", 5, doc! {"errorsOnly": true}, false, 0),
        ];
        for (label, n, extra, want_cursor, want_first) in cases {
            let reply = bulk(c, n, extra);
            let cur = reply.get_document("cursor").unwrap();
            let id = cur.get_i64("id").unwrap();
            assert_eq!(id != 0, want_cursor, "{label}: id={id}");
            assert_eq!(
                cur.get_array("firstBatch").unwrap().len(),
                want_first,
                "{label}"
            );
        }

        // The remainder pages out through `getMore`, addressed by the command
        // namespace, and the cursor closes once drained.
        // `dispatch_full`, not `dispatch`: the batch is handed back out of band
        // through `ctx.pending_batch` (the real server streams it), so bare
        // `dispatch` returns the cursor envelope with no `nextBatch` at all --
        // which reads exactly like an empty cursor if you assert on it.
        let reply = bulk(c, 5, doc! {"cursor": {"batchSize": 2}});
        let id = reply.get_document("cursor").unwrap().get_i64("id").unwrap();
        let more = dispatch_full(&doc! {"getMore": id, "collection": "$cmd.bulkWrite"}, c);
        let cur = more
            .get_document("cursor")
            .unwrap_or_else(|_| panic!("getMore reply: {more:?}"));
        assert_eq!(cur.get_array("nextBatch").unwrap().len(), 3);
        assert_eq!(cur.get_i64("id").unwrap(), 0, "drained, so it closes");
    });
}

fn first_write_error(reply: &Document) -> Document {
    reply
        .get_array("writeErrors")
        .unwrap_or_else(|_| panic!("no writeErrors in {reply}"))[0]
        .as_document()
        .unwrap()
        .clone()
}

/// An `_id` mongod cannot hold is refused on insert and on upsert. An array
/// and a regex were both stored (mongod 8.2.11, 2026-10-10).
#[test]
fn an_array_or_regex_id_is_refused() {
    with_wt(|c| {
        let regex = Bson::RegularExpression(bson::Regex {
            pattern: "a".into(),
            options: String::new(),
        });
        for (value, type_name) in [(bson::bson!([1, 2]), "array"), (regex, "regex")] {
            let reply = dispatch(
                &doc! {"insert": "c", "documents": [{"_id": value.clone()}]},
                c,
            );
            let err = first_write_error(&reply);
            assert_eq!(err.get_i32("code").unwrap(), 53, "{reply}");
            assert_eq!(
                err.get_str("errmsg").unwrap(),
                format!("The '_id' value cannot be of type {type_name}")
            );
            let reply = dispatch(
                &doc! {"update": "c", "updates": [
                    {"q": {"k": 1}, "u": {"$set": {"_id": value.clone()}}, "upsert": true}
                ]},
                c,
            );
            let err = first_write_error(&reply);
            assert_eq!(err.get_i32("code").unwrap(), 53, "{reply}");
            assert_eq!(
                err.get_str("errmsg").unwrap(),
                format!(
                    "Plan executor error during update :: caused by :: The '_id' value cannot \
                     be of type {type_name}"
                )
            );
        }
        // A replacement that would insert an array `_id` is the immutable-field error.
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {"k": 1}, "u": {"_id": [1], "z": 1}, "upsert": true}
            ]},
            c,
        );
        assert_eq!(
            first_write_error(&reply).get_i32("code").unwrap(),
            54,
            "{reply}"
        );
        assert_eq!(count(c), 0);
    });
}

/// An upsert that lands on a taken `_id` names the collection, the index and
/// the key, as every other duplicate key does. It answered a bare
/// `E11000 duplicate key error`.
#[test]
fn an_upsert_onto_a_taken_id_reports_the_key() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {"u": 5}, "u": {"$set": {"_id": 1}}, "upsert": true}
            ]},
            c,
        );
        let err = first_write_error(&reply);
        assert_eq!(err.get_i32("code").unwrap(), 11000);
        assert_eq!(
            err.get_str("errmsg").unwrap(),
            "Plan executor error during update :: caused by :: E11000 duplicate key error \
             collection: t.c index: _id_ dup key: { _id: 1 }"
        );
        assert_eq!(err.get_document("keyPattern").unwrap(), &doc! {"_id": 1});
        assert_eq!(err.get_document("keyValue").unwrap(), &doc! {"_id": 1});
    });
}

/// An update statement's constants (`c`) are variables for a pipeline update,
/// and `upsertSupplied` inserts `c.new` as it stands. Both were ignored.
#[test]
fn update_constants_and_upsert_supplied() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {"_id": 1}, "u": [{"$set": {"b": "$$k"}}], "c": {"k": 7}}
            ]},
            c,
        );
        assert_eq!(reply.get_i32("nModified").unwrap(), 1, "{reply}");

        // Constants on anything but a pipeline are refused, per statement.
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {"_id": 1}, "u": {"$set": {"b": 1}}, "c": {"k": 7}}
            ]},
            c,
        );
        let err = first_write_error(&reply);
        assert_eq!(err.get_i32("code").unwrap(), 51198);
        assert_eq!(
            err.get_str("errmsg").unwrap(),
            "Constant values may only be specified for pipeline updates"
        );

        let reply = dispatch(
            &doc! {"update": "c", "updates": [{
                "q": {"_id": 60}, "u": [{"$set": {"a": 1}}], "upsert": true,
                "upsertSupplied": true, "c": {"new": {"_id": 60, "z": 1}},
            }]},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1, "{reply}");
        assert!(reply.get_array("upserted").is_ok(), "{reply}");
        let found = dispatch_full(&doc! {"find": "c", "filter": {}, "sort": {"_id": 1}}, c);
        let docs = fb(&found, c);
        assert_eq!(
            docs,
            vec![
                Bson::Document(doc! {"_id": 1, "b": 7}),
                Bson::Document(doc! {"_id": 60, "z": 1}),
            ]
        );
    });
}

fn bulk(c: &mut CommandContext, body: Document) -> Document {
    c.db_name = "admin".into();
    let mut cmd = doc! {"bulkWrite": 1};
    cmd.extend(body);
    let reply = dispatch(&cmd, c);
    c.db_name = "t".into();
    reply
}

fn bulk_results(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("no cursor in {reply}"))
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|b| b.as_document().unwrap().clone())
        .collect()
}

/// `bulkWrite` checks the whole command before it writes anything. A
/// malformed later op used to leave the earlier ops applied (mongod 8.2.11,
/// 2026-10-10).
#[test]
fn bulk_write_checks_every_op_before_writing_any() {
    with_wt(|c| {
        let ns = bson::bson!([{"ns": "t.c"}]);
        let good = doc! {"insert": 0, "document": {"_id": 1}};
        let cases: Vec<(Document, i32, &str)> = vec![
            (
                doc! {"insert": 0},
                40414,
                "BSON field 'bulkWrite.ops.document' is missing but a required field",
            ),
            (
                doc! {"delete": 0, "filter": {}, "bogus": 1},
                40415,
                "BSON field 'bulkWrite.ops.bogus' is an unknown field.",
            ),
            (
                doc! {"document": {}, "insert": 0},
                40415,
                "BSON field 'bulkWrite.document' is an unknown field.",
            ),
            (
                doc! {"insert": 5, "document": {"_id": 2}},
                2,
                "BulkWrite ops entry { insert: 5, document: { _id: 2 } } has an invalid nsInfo \
                 index.",
            ),
            (
                doc! {"insert": -1, "document": {}},
                2,
                "BSON field 'insert' value must be >= 0, actual value '-1'",
            ),
            (
                doc! {"update": 0, "filter": {}, "updateMods": 5},
                9,
                "Update argument must be either an object or an array",
            ),
            (
                doc! {"update": 0, "updateMods": {"$set": {"a": 1}}},
                40414,
                "BSON field 'bulkWrite.ops.filter' is missing but a required field",
            ),
            (
                doc! {"delete": 0, "filter": {}, "hint": 5},
                9,
                "Hint must be a string or an object",
            ),
            (
                doc! {"delete": 0, "filter": {}, "multi": 1},
                14,
                "BSON field 'bulkWrite.ops.multi' is the wrong type 'int', expected type 'bool'",
            ),
        ];
        for (bad, code, errmsg) in cases {
            let reply = bulk(
                c,
                doc! {"ops": [good.clone(), bad.clone()], "nsInfo": ns.clone()},
            );
            assert_eq!(reply.get_i32("code").ok(), Some(code), "{bad} -> {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), errmsg, "{bad}");
            assert_eq!(count(c), 0, "{bad} wrote something");
        }
        // A namespace no write may touch fails the command when an op uses it,
        // and is harmless when none does.
        let two = bson::bson!([{"ns": "t.c"}, {"ns": "t.system.views"}]);
        let reply = bulk(
            c,
            doc! {"ops": [good.clone(), {"insert": 1, "document": {"_id": "x"}}], "nsInfo": two.clone()},
        );
        assert_eq!(reply.get_i32("code").ok(), Some(73), "{reply}");
        assert_eq!(count(c), 0);
        let reply = bulk(c, doc! {"ops": [good.clone()], "nsInfo": two});
        assert_eq!(reply.get_i32("nInserted").unwrap(), 1, "{reply}");
        // Every nsInfo entry must be a namespace, used or not.
        let reply = bulk(
            c,
            doc! {"ops": [{"insert": 0, "document": {"_id": 2}}],
            "nsInfo": [{"ns": "t.c"}, {"ns": "nodot"}]},
        );
        assert_eq!(
            reply.get_str("errmsg").unwrap(),
            "Invalid namespace specified for bulkWrite: 'nodot'"
        );
        assert_eq!(count(c), 1);
    });
}

/// What a failed op's entry carries, and that a view takes no writes through
/// `bulkWrite` either (it did: the ops bypass the dispatch-level refusal).
#[test]
fn bulk_write_reports_each_failed_op_as_mongod_does() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        dispatch(&doc! {"create": "vw", "viewOn": "c", "pipeline": []}, c);
        dispatch(
            &doc! {"create": "d", "validator": {"v": {"$type": "int"}}},
            c,
        );
        let ns = bson::bson!([{"ns": "t.c"}, {"ns": "t.vw"}, {"ns": "t.d"}]);

        let reply = bulk(
            c,
            doc! {"ordered": false, "nsInfo": ns.clone(), "ops": [
                {"insert": 1, "document": {"_id": 9}},
                {"update": 1, "filter": {}, "updateMods": {"$set": {"a": 1}}},
                {"insert": 2, "document": {"_id": 9, "v": "no"}},
                {"update": 0, "filter": {"_id": 1}, "updateMods": {"$nope": 1}},
                {"insert": 0, "document": {"_id": 2}},
            ]},
        );
        let results = bulk_results(&reply);
        assert_eq!(reply.get_i32("nErrors").unwrap(), 4, "{reply}");
        assert_eq!(reply.get_i32("nInserted").unwrap(), 1);
        // A view: 166, for the insert and for the update.
        for entry in &results[0..2] {
            assert_eq!(entry.get_i32("code").unwrap(), 166, "{entry}");
            assert_eq!(
                entry.get_str("errmsg").unwrap(),
                "Namespace t.vw is a view, not a collection"
            );
        }
        // A failed update entry ends `n, nModified`; an insert's ends `n`.
        assert_eq!(results[1].keys().last().unwrap(), "nModified");
        assert_eq!(results[0].keys().last().unwrap(), "n");
        assert_eq!(results[3].get_i32("nModified").unwrap(), 0);
        // A failed validation carries its errInfo.
        assert_eq!(results[2].get_i32("code").unwrap(), 121);
        assert!(results[2].get_document("errInfo").is_ok(), "{}", results[2]);
        assert_eq!(count(c), 2);

        // An unacknowledged write reports counters and no per-op results.
        let reply = bulk(
            c,
            doc! {"nsInfo": ns, "writeConcern": {"w": 0}, "ops": [
                {"insert": 0, "document": {"_id": 3}},
                {"insert": 0, "document": {"_id": 3}},
            ]},
        );
        assert!(bulk_results(&reply).is_empty(), "{reply}");
        assert_eq!(reply.get_i32("nErrors").unwrap(), 1);
        assert_eq!(reply.get_i32("nInserted").unwrap(), 1);
    });
}
