//! Real-WiredTiger ports of the admin/DDL command unit tests (create / drop /
//! collMod / listIndexes / createIndexes / explain / stats / rename). Collection
//! options and index duplicates are now real WiredTiger state, verified through
//! `listCollections` / `listIndexes` rather than fake internals.

mod common;

use bson::{doc, Bson, Document};
use common::with_wt;
use secantus_commands::{dispatch, CommandContext};

/// Fetch the `options` sub-doc for collection `name` from `listCollections`.
fn collection_options(c: &mut CommandContext, name: &str) -> Document {
    let lc = dispatch(&doc! {"listCollections": 1}, c);
    lc.get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .filter_map(Bson::as_document)
        .find(|e| e.get_str("name") == Ok(name))
        .unwrap()
        .get_document("options")
        .cloned()
        .unwrap_or_default()
}

fn index_names(c: &mut CommandContext, coll: &str) -> Vec<String> {
    dispatch(&doc! {"listIndexes": coll}, c)
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|b| {
            b.as_document()
                .unwrap()
                .get_str("name")
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn create_index_numeric_direction_is_idempotent() {
    // mongocxx's GridFS pre-creates its indexes with Double directions
    // ({filename: 1.0}); re-creating the same name with Int directions ({...: 1})
    // must be a no-op, not an IndexKeySpecsConflict (mongo-cxx-driver "gridfs does
    // not create additional indexes").
    with_wt(|c| {
        let pre = dispatch(
            &doc! {"createIndexes": "fs.files", "indexes": [
                {"key": {"filename": 1.0, "uploadDate": 1.0}, "name": "filename_1_uploadDate_1"}
            ]},
            c,
        );
        assert_eq!(pre.get_f64("ok").unwrap(), 1.0, "{pre:?}");
        // Same name + numerically-equal Int directions → no-op success.
        let r = dispatch(
            &doc! {"createIndexes": "fs.files", "indexes": [
                {"key": {"filename": 1, "uploadDate": 1}, "name": "filename_1_uploadDate_1"}
            ]},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{r:?}");
        assert_eq!(r.get_str("note").unwrap(), "all indexes already exist");
        // Exactly _id_ + the one index — no additional index created.
        assert_eq!(index_names(c, "fs.files").len(), 2);
        // A genuinely different direction ({filename: -1}) still conflicts (86).
        let conflict = dispatch(
            &doc! {"createIndexes": "fs.files", "indexes": [
                {"key": {"filename": -1, "uploadDate": 1}, "name": "filename_1_uploadDate_1"}
            ]},
            c,
        );
        assert_eq!(conflict.get_f64("ok").unwrap(), 0.0);
        assert_eq!(conflict.get_i32("code").unwrap(), 86);
    });
}

#[test]
fn create_compound_geo_scalar_index() {
    // mongod accepts a compound 2dsphere+scalar index ({g:"2dsphere", z:1}); it is
    // indexed geo-only (the trailing scalar is ignored at index time, verified
    // post-fetch), the derived name is g_2dsphere_z_1, and inserting a geo doc
    // maintains the index cleanly (mongo-php-library CreateIndexesFunctionalTest).
    with_wt(|c| {
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"g": "2dsphere", "z": 1}, "name": "g_2dsphere_z_1"}]},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{r:?}");
        assert!(index_names(c, "c").contains(&"g_2dsphere_z_1".to_string()));
        // A 2d compound index is likewise accepted.
        let r2d = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"p": "2d", "z": 1}, "name": "p_2d_z_1"}]},
            c,
        );
        assert_eq!(r2d.get_f64("ok").unwrap(), 1.0, "{r2d:?}");
        // Inserting a doc with a geo value maintains the compound geo index.
        let ins = dispatch(
            &doc! {"insert": "c", "documents": [
                {"_id": 1, "g": {"type": "Point", "coordinates": [1.0, 2.0]}, "p": [1.0, 2.0], "z": 5}
            ]},
            c,
        );
        assert_eq!(ins.get_f64("ok").unwrap(), 1.0, "{ins:?}");
    });
}

#[test]
fn create_indexes_validates_options() {
    // mongo-ruby-driver index-option specs: commitQuorum / wildcardProjection
    // validation, falsy-hidden stripping, and listIndexes on a missing namespace.
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);

        // commitQuorum with an unsupported value -> UnknownReplWriteConcern (79).
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}], "commitQuorum": "unsupported-value"},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 79, "{r:?}");
        assert!(r
            .get_str("errmsg")
            .unwrap()
            .contains("No write concern mode named 'unsupported-value'"));

        // wildcardProjection must be a non-empty document.
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"$**": 1}, "name": "w", "wildcardProjection": 5}]},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 67);
        assert!(r
            .get_str("errmsg")
            .unwrap()
            .contains("wildcardProjection must be a non-empty object"));

        // wildcardProjection only on a wildcard ($**) index.
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"x": 1}, "name": "x_1", "wildcardProjection": {"rating": 1}}]},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 67);
        assert!(r
            .get_str("errmsg")
            .unwrap()
            .contains("wildcardProjection is only allowed on wildcard indexes"));

        // hidden: false is dropped, not echoed by listIndexes.
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"h": 1}, "name": "h_1", "hidden": false}]},
            c,
        );
        let h = dispatch(&doc! {"listIndexes": "c"}, c)
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|b| b.as_document().unwrap().clone())
            .find(|ix| ix.get_str("name") == Ok("h_1"))
            .unwrap();
        assert!(
            !h.contains_key("hidden"),
            "hidden:false should not be echoed: {h:?}"
        );

        // listIndexes on a nonexistent collection -> NamespaceNotFound (26).
        let r = dispatch(&doc! {"listIndexes": "nope"}, c);
        assert_eq!(r.get_i32("code").unwrap(), 26, "{r:?}");
        assert!(r.get_str("errmsg").unwrap().contains("ns does not exist"));
    });
}

#[test]
fn server_status_tracks_open_cursor_count() {
    // metrics.cursor.open.total rises while a batched cursor is open and returns
    // to baseline after killCursors (mongo-php-driver cursor-destruct-001).
    with_wt(|c| {
        let open_total = |c: &mut CommandContext| -> i64 {
            dispatch(&doc! {"serverStatus": 1}, c)
                .get_document("metrics")
                .unwrap()
                .get_document("cursor")
                .unwrap()
                .get_document("open")
                .unwrap()
                .get_i64("total")
                .unwrap()
        };
        dispatch(
            &doc! {"insert": "c", "documents": (0..5).map(|i| Bson::Document(doc!{"_id": i})).collect::<Vec<_>>()},
            c,
        );
        let base = open_total(c);
        let reply = dispatch(&doc! {"find": "c", "batchSize": 2}, c);
        let cid = reply.get_document("cursor").unwrap().get_i64("id").unwrap();
        assert_ne!(cid, 0, "batched cursor should stay open");
        assert_eq!(open_total(c), base + 1, "count rises while cursor open");
        dispatch(&doc! {"killCursors": "c", "cursors": [cid]}, c);
        assert_eq!(open_total(c), base, "count returns to baseline after kill");
    });
}

#[test]
fn list_indexes_honours_cursor_batch_size() {
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"},
                {"key": {"b": 1}, "name": "b_1"},
            ]},
            c,
        );
        // batchSize 2 over three indexes (_id_, a_1, b_1) ⇒ 2 + live cursor.
        let li = dispatch(&doc! {"listIndexes": "c", "cursor": {"batchSize": 2}}, c);
        let cur = li.get_document("cursor").unwrap();
        assert_eq!(cur.get_array("firstBatch").unwrap().len(), 2);
        assert_ne!(
            cur.get_i64("id").unwrap(),
            0,
            "remaining index ⇒ live cursor"
        );
        // No batchSize ⇒ all three in one batch, cursor closed.
        let li = dispatch(&doc! {"listIndexes": "c"}, c);
        let cur = li.get_document("cursor").unwrap();
        assert_eq!(cur.get_array("firstBatch").unwrap().len(), 3);
        assert_eq!(cur.get_i64("id").unwrap(), 0);
    });
}

#[test]
fn create_then_drop_collection() {
    with_wt(|c| {
        assert_eq!(
            dispatch(&doc! {"create": "c"}, c).get_f64("ok").unwrap(),
            1.0
        );
        // Re-creating with the same (no) options is a no-op success on mongod
        // 8.2.11; different options are NamespaceExists.
        let reply = dispatch(&doc! {"create": "c"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
        let reply = dispatch(&doc! {"create": "c", "capped": true, "size": 4096}, c);
        assert_eq!(reply.get_i32("code").unwrap(), 48);
        let reply = dispatch(&doc! {"drop": "c"}, c);
        assert_eq!(reply.get_str("ns").unwrap(), "t.c");
        // drop again ⇒ idempotent success, no ns.
        let reply = dispatch(&doc! {"drop": "c"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(!reply.contains_key("ns"));
    });
}

#[test]
fn list_collections_returns_created() {
    with_wt(|c| {
        dispatch(&doc! {"create": "a"}, c);
        dispatch(&doc! {"create": "b"}, c);
        let reply = dispatch(&doc! {"listCollections": 1}, c);
        let mut names: Vec<String> = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|b| {
                b.as_document()
                    .unwrap()
                    .get_str("name")
                    .unwrap()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    });
}

#[test]
fn create_stores_validator() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"create": "c", "validator": {"a": {"$exists": true}}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(collection_options(c, "c").contains_key("validator"));
    });
}

#[test]
fn clustered_index_create_list_and_validation() {
    with_wt(|c| {
        // Valid: stored normalised, surfaced in listCollections (no idIndex),
        // and listIndexes reports a single clustered entry under the user's name.
        let reply = dispatch(
            &doc! {"create": "c",
            "clusteredIndex": {"key": {"_id": 1}, "unique": true, "name": "ci"}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);

        let lc = dispatch(&doc! {"listCollections": 1}, c);
        let spec = lc
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .filter_map(Bson::as_document)
            .find(|e| e.get_str("name") == Ok("c"))
            .unwrap()
            .clone();
        assert!(spec
            .get_document("options")
            .unwrap()
            .contains_key("clusteredIndex"));
        assert!(!spec.contains_key("idIndex"));

        let li = dispatch(&doc! {"listIndexes": "c"}, c);
        let idx = li
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(idx.len(), 1);
        let first = idx[0].as_document().unwrap();
        assert_eq!(first.get_str("name"), Ok("ci"));
        assert_eq!(first.get_bool("clustered"), Ok(true));

        // Invalid specs are rejected.
        let bad_key = dispatch(
            &doc! {"create": "b1", "clusteredIndex": {"key": {"x": 1}, "unique": true}},
            c,
        );
        assert_eq!(bad_key.get_f64("ok").unwrap(), 0.0);
        assert_eq!(bad_key.get_i32("code").unwrap(), 197);
        let bad_uniq = dispatch(
            &doc! {"create": "b2", "clusteredIndex": {"key": {"_id": 1}}},
            c,
        );
        assert_eq!(bad_uniq.get_f64("ok").unwrap(), 0.0);
        assert_eq!(bad_uniq.get_i32("code").unwrap(), 5979700);
    });
}

#[test]
fn collmod_sets_validator() {
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        let reply = dispatch(
            &doc! {"collMod": "c", "validator": {"n": {"$gt": 0}},
            "changeStreamPreAndPostImages": {"enabled": true}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let opts = collection_options(c, "c");
        assert!(opts.contains_key("validator"));
        assert!(opts.contains_key("changeStreamPreAndPostImages"));
    });
}

#[test]
fn collmod_index_prepare_unique_then_unique_conversion() {
    // prepareUnique succeeds over pre-existing real duplicates; unique:true then
    // refuses with 359 + violations; after the duplicate is removed it succeeds.
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        // Two docs sharing x=1 — a real duplicate group on the x index.
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            c,
        );
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"x": 1}, "name": "x_1"}]},
            c,
        );

        // prepareUnique arms the index — ok even with existing dups.
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"name": "x_1", "prepareUnique": true}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);

        // unique:true with existing duplicates → 359 + violations.
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"name": "x_1", "unique": true}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 359);
        assert_eq!(r.get_str("codeName").unwrap(), "CannotConvertIndexToUnique");
        let v = r.get_array("violations").unwrap();
        assert_eq!(
            v[0].as_document().unwrap().get_array("ids").unwrap(),
            &vec![Bson::Int32(1), Bson::Int32(2)]
        );

        // Remove the duplicate → conversion now succeeds.
        dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"_id": 2}, "limit": 1}]},
            c,
        );
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"name": "x_1", "unique": true}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);

        // A missing index → IndexNotFound (27).
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"name": "nope", "unique": true}},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 27);
    });
}

#[test]
fn collmod_index_expire_after_seconds_reflection() {
    // collMod retuning a TTL index echoes expireAfterSeconds_old/new and persists
    // the new expiry — php-lib ModifyCollectionFunctionalTest::testCollMod.
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"lastAccess": 1}, "expireAfterSeconds": 3, "name": "lastAccess_1"}
            ]},
            c,
        );
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"keyPattern": {"lastAccess": 1}, "expireAfterSeconds": 1000}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        // Both are int64 on mongod 8.2.11 (this asserted int32 until 2026-10-10).
        assert_eq!(r.get_i64("expireAfterSeconds_old").unwrap(), 3);
        assert_eq!(r.get_i64("expireAfterSeconds_new").unwrap(), 1000);
        // Persisted: listIndexes reports the new expiry.
        let li = dispatch(&doc! {"listIndexes": "c"}, c);
        let idx = li
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|b| b.as_document().unwrap().clone())
            .find(|d| d.get_str("name") == Ok("lastAccess_1"))
            .unwrap();
        assert_eq!(idx.get_i32("expireAfterSeconds").unwrap(), 1000);
    });
}

#[test]
fn collmod_missing_ns_is_namespace_not_found() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"collMod": "nope", "validator": {}}, c);
        assert_eq!(reply.get_i32("code").unwrap(), 26);
        assert_eq!(reply.get_str("codeName").unwrap(), "NamespaceNotFound");
    });
}

#[test]
fn explain_find_collscan_shape() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"explain": {"find": "c", "filter": {"x": 1}}}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let qp = reply.get_document("queryPlanner").unwrap();
        assert_eq!(qp.get_str("namespace").unwrap(), "t.c");
        let wp = qp.get_document("winningPlan").unwrap();
        assert_eq!(wp.get_str("stage").unwrap(), "COLLSCAN");
        assert!(reply.get_document("executionStats").is_ok());
    });
}

#[test]
fn explain_query_planner_verbosity_omits_exec_stats() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"explain": {"find": "c"}, "verbosity": "queryPlanner"},
            c,
        );
        assert!(reply.get_document("queryPlanner").is_ok());
        assert!(reply.get("executionStats").is_none());
    });
}

#[test]
fn explain_invalid_verbosity_is_bad_value() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"explain": {"find": "c"}, "verbosity": "bogus"}, c);
        assert_eq!(reply.get_i32("code").unwrap(), 2);
        assert_eq!(reply.get_str("codeName").unwrap(), "BadValue");
    });
}

#[test]
fn explain_with_majority_write_concern_rejected() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"explain": {"find": "c"}, "writeConcern": {"w": "majority"}},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 72);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidOptions");
    });
}

#[test]
fn explain_aggregate_has_cursor_stages() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"explain": {"aggregate": "c", "pipeline": [{"$match": {"x": 1}}]}},
            c,
        );
        let stages = reply.get_array("stages").unwrap();
        assert!(stages[0].as_document().unwrap().contains_key("$cursor"));
    });
}

#[test]
fn explain_wrapped_commands_across_verbosities() {
    // mongo-php-library ExplainFunctionalTest wraps count/delete/update/distinct/
    // findAndModify and asserts, per verbosity: queryPlanner always present;
    // executionStats present except at queryPlanner; allPlansExecution present
    // only at allPlansExecution. (explain is a dry run — it never mutates.)
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 11}, {"_id": 2, "x": 22}]},
            c,
        );
        let wrapped = vec![
            doc! {"count": "c", "query": {"x": 11}},
            doc! {"delete": "c", "deletes": [{"q": {"x": 11}, "limit": 1}]},
            doc! {"update": "c", "updates": [{"q": {"x": 11}, "u": {"$set": {"y": 1}}}]},
            doc! {"distinct": "c", "key": "x"},
            doc! {"findAndModify": "c", "query": {"x": 11}, "update": {"$set": {"y": 2}}},
        ];
        for inner in wrapped {
            let qp = dispatch(
                &doc! {"explain": inner.clone(), "verbosity": "queryPlanner"},
                c,
            );
            assert_eq!(qp.get_f64("ok").unwrap(), 1.0, "{inner:?}");
            assert!(qp.get_document("queryPlanner").is_ok(), "{inner:?}");
            assert!(qp.get("executionStats").is_none(), "{inner:?}");

            let es = dispatch(
                &doc! {"explain": inner.clone(), "verbosity": "executionStats"},
                c,
            );
            let stats = es.get_document("executionStats").unwrap();
            assert!(stats.get("allPlansExecution").is_none(), "{inner:?}");

            let ap = dispatch(
                &doc! {"explain": inner.clone(), "verbosity": "allPlansExecution"},
                c,
            );
            let stats = ap.get_document("executionStats").unwrap();
            assert!(stats.get_array("allPlansExecution").is_ok(), "{inner:?}");
        }
    });
}

#[test]
fn create_indexes_and_list() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"},
                // mongod requires `name` (8.2.11); drivers derive it.
                {"key": {"b": -1}, "name": "b_-1"},
            ]},
            c,
        );
        assert!(reply.get_bool("createdCollectionAutomatically").unwrap());
        // The collection the build creates already has its `_id` index when
        // mongod counts (this asserted 0 until 2026-10-10).
        assert_eq!(reply.get_i32("numIndexesBefore").unwrap(), 1);
        assert_eq!(reply.get_i32("numIndexesAfter").unwrap(), 3);
        assert_eq!(index_names(c, "c"), vec!["_id_", "a_1", "b_-1"]);
    });
}

#[test]
fn create_index_conflicts_and_noop_note() {
    with_wt(|c| {
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            c,
        );
        // Identical re-create → no-op success with the note drivers key off.
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert_eq!(r.get_str("note").unwrap(), "all indexes already exist");
        // Same name, different key spec → IndexKeySpecsConflict (86).
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"b": 1}, "name": "a_1"}]},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 86);
        assert_eq!(r.get_str("codeName").unwrap(), "IndexKeySpecsConflict");
        // Same name + key, different option → IndexOptionsConflict (85).
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1", "unique": true}
            ]},
            c,
        );
        // Same name, different options: mongod 8.2.11 answers 86.
        assert_eq!(r.get_i32("code").unwrap(), 86);
        assert_eq!(r.get_str("codeName").unwrap(), "IndexKeySpecsConflict");
    });
}

#[test]
fn create_text_index_is_cannot_create_index() {
    with_wt(|c| {
        let r = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"t": "text"}, "name": "t_text"}]},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 67);
        assert_eq!(r.get_str("codeName").unwrap(), "CannotCreateIndex");
    });
}

#[test]
fn drop_indexes_by_name_and_star() {
    with_wt(|c| {
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"}, {"key": {"b": 1}, "name": "b_1"}
            ]},
            c,
        );
        let reply = dispatch(&doc! {"dropIndexes": "c", "index": "a_1"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        // unknown index ⇒ IndexNotFound
        assert_eq!(
            dispatch(&doc! {"dropIndexes": "c", "index": "zzz"}, c)
                .get_i32("code")
                .unwrap(),
            27
        );
        // "*" drops the rest; only _id_ remains.
        dispatch(&doc! {"dropIndexes": "c", "index": "*"}, c);
        assert_eq!(index_names(c, "c"), vec!["_id_"]);
    });
}

#[test]
fn server_status_minimal_shape() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"serverStatus": 1}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(
            reply.get_str("version").unwrap(),
            secantus_commands::SERVER_VERSION
        );
        assert_eq!(reply.get_str("process").unwrap(), "mongod");
        let marker = reply.get_document("secantus").unwrap();
        assert_eq!(marker.get_str("server").unwrap(), "rust");
        assert!(!marker.get_str("version").unwrap().is_empty());
    });
}

#[test]
fn drop_database_reports_dropped() {
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        let reply = dispatch(&doc! {"dropDatabase": 1}, c);
        assert_eq!(reply.get_str("dropped").unwrap(), "t");
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn db_stats_counts_collections() {
    with_wt(|c| {
        dispatch(&doc! {"create": "a"}, c);
        dispatch(&doc! {"create": "b"}, c);
        let reply = dispatch(&doc! {"dbStats": 1}, c);
        assert_eq!(reply.get_str("db").unwrap(), "t");
        assert_eq!(reply.get_i32("collections").unwrap(), 2);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn dbstats_lowercase_alias_is_recognised() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"dbstats": 1}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(reply.get_str("db").unwrap(), "t");
    });
}

#[test]
fn coll_stats_shape() {
    with_wt(|c| {
        dispatch(&doc! {"create": "c"}, c);
        let reply = dispatch(&doc! {"collStats": "c"}, c);
        assert_eq!(reply.get_str("ns").unwrap(), "t.c");
        assert!(reply.get("indexSizes").is_some());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn rename_collection_ok() {
    with_wt(|c| {
        // Real rename needs a real source collection.
        dispatch(&doc! {"create": "a"}, c);
        // renameCollection runs against `admin` (mongod 8.2.11).
        c.db_name = "admin".to_string();
        let reply = dispatch(&doc! {"renameCollection": "t.a", "to": "t.b"}, c);
        c.db_name = "t".to_string();
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(index_names(c, "b"), vec!["_id_"]);
    });
}

#[test]
fn rename_nonexistent_source_is_namespace_not_found() {
    // Renaming a missing source is NamespaceNotFound (26), not NamespaceExists
    // (48) — php-lib RenameCollectionFunctionalTest::testRenameNonexistentCollection.
    with_wt(|c| {
        c.db_name = "admin".to_string();
        let r = dispatch(&doc! {"renameCollection": "t.nope", "to": "t.dst"}, c);
        c.db_name = "t".to_string();
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 26);
        assert_eq!(r.get_str("codeName").unwrap(), "NamespaceNotFound");
    });
}

// --- capped collections: every expectation is what mongod 8.2.11 answered
// --- for the same command (2026-10-09).

fn create_capped(c: &mut CommandContext, name: &str, extra: Document) -> Document {
    let mut cmd = doc! {"create": name};
    cmd.extend(extra);
    dispatch(&cmd, c)
}

#[test]
fn capped_size_and_max_are_stored_as_integers() {
    with_wt(|c| {
        // pymongo sends `size` as a double; it used to be echoed as one.
        for (name, extra, want) in [
            (
                "a",
                doc! {"capped": true, "size": 1000.7_f64},
                doc! {"capped": true, "size": 1000_i32},
            ),
            (
                "b",
                doc! {"capped": 1_i32, "size": 1000_i64, "max": 3.7_f64},
                doc! {"capped": true, "size": 1000_i32, "max": 3_i32},
            ),
            (
                "c",
                doc! {"capped": true, "size": 1_i64 << 50},
                doc! {"capped": true, "size": 1_i64 << 50},
            ),
            // Zero or less means no document limit, stored as 2147483647.
            (
                "d",
                doc! {"capped": true, "size": 1000_i32, "max": 0_i32},
                doc! {"capped": true, "size": 1000_i32, "max": i32::MAX},
            ),
            (
                "e",
                doc! {"capped": true, "size": 1000_i32, "max": -5_i64},
                doc! {"capped": true, "size": 1000_i32, "max": i32::MAX},
            ),
            (
                "f",
                doc! {"capped": true, "size": 10_i32, "max": Bson::Null},
                doc! {"capped": true, "size": 10_i32},
            ),
        ] {
            let reply = create_capped(c, name, extra.clone());
            assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{extra}: {reply}");
            assert_eq!(collection_options(c, name), want, "{extra}");
        }
        // Creating it again with options that normalise to the same thing is
        // not a conflict; a different size is.
        let again = create_capped(
            c,
            "d",
            doc! {"capped": true, "size": 1000.9_f64, "max": -7_i32},
        );
        assert_eq!(again.get_f64("ok").unwrap(), 1.0, "{again}");
        let other = create_capped(c, "d", doc! {"capped": true, "size": 2000_i32});
        assert_eq!(other.get_i32("code").unwrap(), 48, "{other}");
    });
}

#[test]
fn capped_size_and_max_out_of_range_are_refused() {
    with_wt(|c| {
        for (extra, msg) in [
            (
                doc! {"capped": true, "size": 0_i32},
                "BSON field 'size' value must be >= 1, actual value '0'",
            ),
            (
                doc! {"capped": true, "size": -1_i32},
                "BSON field 'size' value must be >= 1, actual value '-1'",
            ),
            (
                doc! {"capped": true, "size": 0.5_f64},
                "BSON field 'size' value must be >= 1, actual value '0'",
            ),
            (
                doc! {"capped": true, "size": f64::NAN},
                "BSON field 'size' value must be >= 1, actual value '0'",
            ),
            (
                doc! {"capped": true, "size": 1_i64 << 62},
                "BSON field 'size' value must be <= 1125899906842624, actual value \
                 '4611686018427387904'",
            ),
            (
                doc! {"capped": true, "size": f64::INFINITY},
                "BSON field 'size' value must be <= 1125899906842624, actual value \
                 '9223372036854775807'",
            ),
            (
                doc! {"capped": true, "size": 1000_i32, "max": 1_i64 << 31},
                "BSON field 'max' value must be < 2147483648, actual value '2147483648'",
            ),
            // The range is checked before the rules about `capped` itself.
            (
                doc! {"size": 0_i32},
                "BSON field 'size' value must be >= 1, actual value '0'",
            ),
            (
                doc! {"capped": true, "max": 1_i64 << 31},
                "BSON field 'max' value must be < 2147483648, actual value '2147483648'",
            ),
        ] {
            let reply = create_capped(c, "bad", extra.clone());
            assert_eq!(reply.get_i32("code").unwrap(), 2, "{extra}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{extra}");
        }
        assert!(!dispatch(&doc! {"listCollections": 1}, c)
            .to_string()
            .contains("\"bad\""));
    });
}

#[test]
fn coll_mod_rebounds_a_capped_collection() {
    with_wt(|c| {
        create_capped(
            c,
            "s",
            doc! {"capped": true, "size": 1000_i32, "max": 5_i32},
        );
        dispatch(&doc! {"create": "plain"}, c);
        let reply = dispatch(
            &doc! {"collMod": "s", "cappedSize": 7000.9_f64, "cappedMax": 2_i64},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
        assert_eq!(
            collection_options(c, "s"),
            doc! {"capped": true, "size": 7000_i32, "max": 2_i32}
        );
        dispatch(&doc! {"collMod": "s", "cappedMax": 0_i32}, c);
        assert_eq!(collection_options(c, "s").get_i32("max").unwrap(), i32::MAX);
        // A null is "not given".
        dispatch(&doc! {"collMod": "s", "cappedSize": Bson::Null}, c);
        assert_eq!(collection_options(c, "s").get_i32("size").unwrap(), 7000);

        for (cmd, code, msg) in [
            (
                doc! {"collMod": "s", "cappedSize": 0_i32},
                2,
                "BSON field 'cappedSize' value must be >= 1, actual value '0'",
            ),
            (
                doc! {"collMod": "s", "cappedSize": 1_i64 << 62},
                2,
                "BSON field 'cappedSize' value must be <= 1125899906842624, actual value \
                 '4611686018427387904'",
            ),
            (
                doc! {"collMod": "s", "cappedMax": 1_i64 << 31},
                2,
                "BSON field 'cappedMax' value must be < 2147483648, actual value '2147483648'",
            ),
            (
                doc! {"collMod": "s", "cappedSize": "x"},
                14,
                "BSON field 'collMod.cappedSize' is the wrong type 'string', expected types \
                 '[double, int, long, decimal]'",
            ),
            (
                doc! {"collMod": "plain", "cappedSize": 5000_i32},
                72,
                "Collection must be capped.",
            ),
            (
                doc! {"collMod": "plain", "cappedMax": 5_i32},
                72,
                "Collection must be capped.",
            ),
            // The range check comes before the capped check.
            (
                doc! {"collMod": "plain", "cappedSize": 0_i32},
                2,
                "BSON field 'cappedSize' value must be >= 1, actual value '0'",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        assert_eq!(collection_options(c, "s").get_i32("size").unwrap(), 7000);
    });
}

#[test]
fn coll_stats_reports_a_capped_collections_bounds() {
    with_wt(|c| {
        create_capped(
            c,
            "s",
            doc! {"capped": true, "size": 1000_i32, "max": 5_i32},
        );
        create_capped(c, "nomax", doc! {"capped": true, "size": 1000_i32});
        create_capped(
            c,
            "unlimited",
            doc! {"capped": true, "size": 1000_i32, "max": 0_i32},
        );
        dispatch(&doc! {"create": "plain"}, c);
        for (name, max) in [("s", 5), ("nomax", 0), ("unlimited", i32::MAX)] {
            let stats = dispatch(&doc! {"collStats": name}, c);
            assert_eq!(stats.get("max"), Some(&Bson::Int32(max)), "{name}: {stats}");
            assert_eq!(stats.get("maxSize"), Some(&Bson::Int32(1000)), "{name}");
            let agg = dispatch(
                &doc! {"aggregate": name, "pipeline": [{"$collStats": {"storageStats": {}}}],
                "cursor": {}},
                c,
            );
            let first = agg
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()[0]
                .as_document()
                .unwrap()
                .get_document("storageStats")
                .unwrap()
                .clone();
            assert_eq!(first.get("max"), Some(&Bson::Int32(max)), "{name}: {first}");
            assert_eq!(first.get("maxSize"), Some(&Bson::Int32(1000)), "{name}");
        }
        let plain = dispatch(&doc! {"collStats": "plain"}, c);
        assert!(!plain.get_bool("capped").unwrap());
        assert!(
            !plain.contains_key("max") && !plain.contains_key("maxSize"),
            "{plain}"
        );
    });
}

#[test]
fn a_write_to_a_capped_collection_inside_a_transaction_is_refused() {
    with_wt(|c| {
        // The registry the server builds: commit and rollback go to storage.
        use secantus_commands::transactions::{Transaction, TransactionRegistry};
        let commit = c.storage.clone().unwrap();
        let rollback = c.storage.clone().unwrap();
        c.transactions = Some(std::sync::Arc::new(TransactionRegistry::new(
            Box::new(move |txn: &mut Transaction| {
                if let Some(h) = txn.handle.as_mut() {
                    let _ = commit.commit_user_transaction(h.as_mut());
                }
            }),
            Box::new(move |txn: &mut Transaction| {
                if let Some(h) = txn.handle.as_mut() {
                    let _ = rollback.rollback_user_transaction(h.as_mut());
                }
            }),
            secantus_commands::transactions::DEFAULT_LIFETIME_SECONDS,
            Box::new(|| 0.0),
        )));
        create_capped(c, "tx", doc! {"capped": true, "size": 100000_i32});
        dispatch(&doc! {"insert": "tx", "documents": [{"_id": 1, "a": 1}]}, c);
        dispatch(&doc! {"insert": "plain", "documents": [{"_id": 1}]}, c);
        let lsid = doc! {"id": bson::Binary {
        subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![7; 16] }};
        let refused = "Collection 't.tx' is a capped collection. Writes in transactions are not \
                       allowed on capped collections.";
        let mut txn_number = 0_i64;
        let mut in_txn = |c: &mut CommandContext, mut cmd: Document| {
            txn_number += 1;
            cmd.insert("lsid", lsid.clone());
            cmd.insert("txnNumber", txn_number);
            cmd.insert("autocommit", false);
            cmd.insert("startTransaction", true);
            let reply = dispatch(&cmd, c);
            // The refusal aborts the transaction: committing it finds none.
            let commit = dispatch(
                &doc! {"commitTransaction": 1, "lsid": lsid.clone(), "txnNumber": txn_number,
                "autocommit": false},
                c,
            );
            (reply, commit.get_i32("code").ok())
        };
        for (cmd, code, msg) in [
            (
                doc! {"insert": "tx", "documents": [{"_id": 2}, {"_id": 3}], "ordered": false},
                263,
                refused,
            ),
            (
                doc! {"update": "tx", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 2}}}]},
                263,
                refused,
            ),
            (
                doc! {"update": "tx", "updates": [{"q": {"_id": 9}, "u": {"$set": {"a": 2}}, "upsert": true}]},
                263,
                refused,
            ),
            (
                doc! {"delete": "tx", "deletes": [{"q": {"_id": 1}, "limit": 1_i32}]},
                20,
                "Cannot remove from a capped collection in a multi-document transaction: t.tx",
            ),
        ] {
            let (reply, commit) = in_txn(c, cmd.clone());
            assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{cmd}: {reply}");
            assert_eq!(reply.get_i32("n").unwrap(), 0, "{cmd}");
            let errors = reply.get_array("writeErrors").unwrap();
            assert_eq!(errors.len(), 1, "{cmd}: {reply}");
            let e = errors[0].as_document().unwrap();
            assert_eq!(
                (e.get_i32("index").unwrap(), e.get_i32("code").unwrap()),
                (0, code),
                "{cmd}"
            );
            assert_eq!(e.get_str("errmsg").unwrap(), msg, "{cmd}");
            assert_eq!(commit, Some(251), "{cmd}");
        }
        let (reply, commit) = in_txn(
            c,
            doc! {"findAndModify": "tx", "query": {"_id": 1}, "update": {"$set": {"a": 3}}},
        );
        assert_eq!(reply.get_i32("code").unwrap(), 263, "{reply}");
        assert_eq!(reply.get_str("errmsg").unwrap(), refused);
        assert_eq!(commit, Some(251));
        // Reading a capped collection in a transaction is fine, and so is
        // writing any other collection.
        let (reply, commit) = in_txn(c, doc! {"find": "tx"});
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
        assert_eq!(commit, None);
        let (reply, commit) = in_txn(c, doc! {"insert": "plain", "documents": [{"_id": 2}]});
        assert_eq!(reply.get_i32("n").unwrap(), 1, "{reply}");
        assert_eq!(commit, None);
        let left = dispatch(&doc! {"find": "tx", "projection": {"_id": 1, "a": 1}}, c);
        assert_eq!(
            left.get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap(),
            &vec![Bson::Document(doc! {"_id": 1, "a": 1})]
        );
    });
}

// --- views: every expectation is what mongod 8.2.11 answered (2026-10-09).

/// `src` (ten documents, `g` = `_id % 3`), `other`, a view `v1` of `g == 1`
/// and a view `v4` over `v1`.
fn seed_views(c: &mut CommandContext) {
    let src: Vec<Bson> = (0..10_i32)
        .map(|i| Bson::Document(doc! {"_id": i, "g": i % 3, "v": i * 10, "tags": [i, i + 1]}))
        .collect();
    dispatch(&doc! {"insert": "src", "documents": src}, c);
    let other: Vec<Bson> = (0..3_i32)
        .map(|i| Bson::Document(doc! {"_id": i, "g": i}))
        .collect();
    dispatch(&doc! {"insert": "other", "documents": other}, c);
    for cmd in [
        doc! {"create": "v1", "viewOn": "src", "pipeline": [{"$match": {"g": 1}}]},
        doc! {"create": "v4", "viewOn": "v1", "pipeline": [{"$project": {"v": 1, "g": 1}}]},
    ] {
        assert_eq!(dispatch(&cmd, c).get_f64("ok").unwrap(), 1.0, "{cmd}");
    }
}

fn agg_docs(c: &mut CommandContext, coll: &str, pipeline: Vec<Document>) -> Vec<Document> {
    let reply = dispatch(
        &doc! {"aggregate": coll, "pipeline": pipeline, "cursor": {}},
        c,
    );
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|b| b.as_document().unwrap().clone())
        .collect()
}

fn view_ids(c: &mut CommandContext, find: Document) -> Vec<i32> {
    // `dispatch_full`: a plain collection's `find` hands its batch over
    // out of band, a view's (an aggregation) inline.
    let reply = common::dispatch_full(&find, c);
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|b| b.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

#[test]
fn a_view_refuses_writes_indexes_and_collection_commands() {
    with_wt(|c| {
        seed_views(c);
        let refused = "Namespace t.v1 is a view, not a collection";
        // Writes: one error per statement, or the first alone when ordered.
        for (cmd, reported) in [
            (
                doc! {"insert": "v1", "documents": [{"_id": 100}, {"_id": 101}]},
                1,
            ),
            (
                doc! {"insert": "v1", "documents": [{"_id": 100}, {"_id": 101}], "ordered": false},
                2,
            ),
            (
                doc! {"update": "v1", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}]},
                1,
            ),
            (
                doc! {"delete": "v1", "ordered": false,
                "deletes": [{"q": {}, "limit": 1_i32}, {"q": {}, "limit": 0_i32}]},
                2,
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{cmd}: {reply}");
            assert_eq!(reply.get_i32("n").unwrap(), 0, "{cmd}");
            let errors = reply.get_array("writeErrors").unwrap();
            assert_eq!(errors.len(), reported, "{cmd}: {reply}");
            for (i, e) in errors.iter().enumerate() {
                let e = e.as_document().unwrap();
                assert_eq!(e.get_i32("index").unwrap(), i as i32);
                assert_eq!(e.get_i32("code").unwrap(), 166);
                assert_eq!(e.get_str("errmsg").unwrap(), refused);
            }
        }
        for (cmd, code, msg) in [
            (
                doc! {"findAndModify": "v1", "query": {}, "remove": true},
                166,
                refused,
            ),
            (
                doc! {"createIndexes": "v1", "indexes": [{"key": {"v": 1}, "name": "v_1"}]},
                166,
                refused,
            ),
            (doc! {"listIndexes": "v1"}, 166, refused),
            (doc! {"dropIndexes": "v1", "index": "*"}, 166, refused),
            (doc! {"collStats": "v1"}, 166, refused),
            (doc! {"validate": "v1"}, 166, "Cannot validate a view"),
            (
                doc! {"aggregate": "v1", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}},
                166,
                "Executor error during aggregate command on namespace: t.v1 :: caused by :: \
                 Namespace t.v1 is a view, not a collection",
            ),
            (
                doc! {"aggregate": "other", "pipeline": [{"$out": "v1"}], "cursor": {}},
                166,
                "Executor error during aggregate command on namespace: t.other :: caused by :: \
                 Namespace t.v1 is a view, not a collection",
            ),
            (
                doc! {"aggregate": "other", "pipeline": [{"$merge": {"into": "v1"}}], "cursor": {}},
                166,
                "Executor error during aggregate command on namespace: t.other :: caused by :: \
                 Namespace t.v1 is a view, not a collection",
            ),
            (
                doc! {"renameCollection": "t.v1", "to": "t.v1b"},
                166,
                "cannot rename view: t.v1",
            ),
            (
                doc! {"renameCollection": "t.other", "to": "t.v1", "dropTarget": true},
                48,
                "a view already exists with that name: t.v1",
            ),
            (
                doc! {"find": "v1", "tailable": true},
                168,
                "Tailable cursors are not supported in aggregation.",
            ),
            (
                doc! {"find": "v1", "collation": {"locale": "fr"}},
                167,
                "Cannot override a view's default collation",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        // Nothing was written anywhere, and the view still reads its source.
        assert_eq!(view_ids(c, doc! {"find": "v1"}), vec![1, 4, 7]);
        assert_eq!(view_ids(c, doc! {"find": "other"}), vec![0, 1, 2]);
        // Dropping a view reports no index count.
        assert_eq!(
            dispatch(&doc! {"drop": "v4"}, c),
            doc! {"ns": "t.v4", "ok": 1.0}
        );
    });
}

#[test]
fn a_view_is_read_through_by_distinct_joins_and_natural_order() {
    with_wt(|c| {
        seed_views(c);
        let distinct = |c: &mut CommandContext, cmd: Document| {
            let reply = dispatch(&cmd, c);
            let mut values: Vec<i32> = reply
                .get_array("values")
                .unwrap()
                .iter()
                .map(|v| v.as_i32().unwrap())
                .collect();
            values.sort();
            (values, reply.get("ok").cloned())
        };
        // mongod answers `ok` as an int32 for a view's distinct.
        assert_eq!(
            distinct(c, doc! {"distinct": "v1", "key": "v"}),
            (vec![10, 40, 70], Some(Bson::Int32(1)))
        );
        assert_eq!(
            distinct(
                c,
                doc! {"distinct": "v1", "key": "v", "query": {"v": {"$gt": 10}}}
            )
            .0,
            vec![40, 70]
        );
        assert_eq!(distinct(c, doc! {"distinct": "v4", "key": "g"}).0, vec![1]);
        assert_eq!(
            distinct(c, doc! {"distinct": "src", "key": "g"}),
            (vec![0, 1, 2], Some(Bson::Double(1.0)))
        );

        let sizes = |docs: Vec<Document>| -> Vec<i32> {
            docs.iter().map(|d| d.get_i32("n").unwrap()).collect()
        };
        let lookup = agg_docs(
            c,
            "other",
            vec![
                doc! {"$lookup": {"from": "v1", "localField": "g", "foreignField": "g", "as": "m"}},
                doc! {"$project": {"n": {"$size": "$m"}}},
                doc! {"$sort": {"_id": 1}},
            ],
        );
        assert_eq!(sizes(lookup), vec![0, 3, 0]);
        let graph = agg_docs(
            c,
            "other",
            vec![
                doc! {"$graphLookup": {"from": "v1", "startWith": "$g", "connectFromField": "g",
                "connectToField": "g", "as": "m", "maxDepth": 0_i32}},
                doc! {"$project": {"n": {"$size": "$m"}}},
                doc! {"$sort": {"_id": 1}},
            ],
        );
        assert_eq!(sizes(graph), vec![0, 3, 0]);
        let union = agg_docs(
            c,
            "other",
            vec![doc! {"$unionWith": "v1"}, doc! {"$count": "n"}],
        );
        assert_eq!(union, vec![doc! {"n": 6}]);

        // `$natural` is the direction the source is scanned in.
        assert_eq!(
            view_ids(c, doc! {"find": "v1", "sort": {"$natural": -1}}),
            vec![7, 4, 1]
        );
        assert_eq!(
            view_ids(c, doc! {"find": "v1", "sort": {"$natural": 1}}),
            vec![1, 4, 7]
        );
    });
}

#[test]
fn coll_mod_redefines_a_view_and_only_a_view() {
    with_wt(|c| {
        seed_views(c);
        // Each of `pipeline` and `viewOn` replaces its own half.
        dispatch(
            &doc! {"collMod": "v1", "pipeline": [{"$match": {"g": 2}}]},
            c,
        );
        assert_eq!(view_ids(c, doc! {"find": "v1"}), vec![2, 5, 8]);
        assert_eq!(
            collection_options(c, "v1"),
            doc! {"viewOn": "src", "pipeline": [{"$match": {"g": 2}}]}
        );
        dispatch(&doc! {"collMod": "v1", "viewOn": "other"}, c);
        assert_eq!(view_ids(c, doc! {"find": "v1"}), vec![2]);
        dispatch(
            &doc! {"collMod": "v1", "viewOn": "src", "pipeline": [{"$match": {"g": 0}}]},
            c,
        );
        assert_eq!(view_ids(c, doc! {"find": "v1"}), vec![0, 3, 6, 9]);

        for (cmd, code, msg) in [
            (
                doc! {"collMod": "src", "viewOn": "other", "pipeline": []},
                72,
                "option only supported on a view: pipeline",
            ),
            (
                doc! {"collMod": "src", "viewOn": "other"},
                72,
                "option only supported on a view: viewOn",
            ),
            (
                doc! {"collMod": "v1", "validator": {"a": 1}},
                72,
                "option not supported on a view: validator",
            ),
            (
                doc! {"collMod": "v1", "validationLevel": "off"},
                72,
                "option not supported on a view: validationLevel",
            ),
            (
                doc! {"collMod": "v1", "index": {"name": "x", "hidden": true}},
                72,
                "option not supported on a view: index",
            ),
            (
                doc! {"collMod": "v1", "viewOn": "v4", "pipeline": []},
                5,
                "View cycle detected: t.v1 => t.v1 => t.v4 => t.v1",
            ),
            (
                doc! {"collMod": "v1", "viewOn": "src", "pipeline": [{"$nope": 1}]},
                40324,
                "Unrecognized pipeline stage name: '$nope'",
            ),
            (
                doc! {"collMod": "v1", "viewOn": "src", "pipeline": [{"$out": "x"}]},
                167,
                "Invalid pipeline for view t.v1 :: caused by :: The aggregation stage $out in \
                 location 0 of the pipeline cannot be used in the view definition of t.v1 because \
                 it writes to disk",
            ),
            (
                doc! {"collMod": "v1", "viewOn": "src", "pipeline": {"a": 1}},
                14,
                "BSON field 'collMod.pipeline' is the wrong type 'object', expected type 'array'",
            ),
            (
                doc! {"collMod": "v1", "viewOn": "", "pipeline": []},
                2,
                "'viewOn' cannot be empty",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        assert_eq!(view_ids(c, doc! {"find": "v1"}), vec![0, 3, 6, 9]);
    });
}

#[test]
fn create_checks_a_views_definition() {
    with_wt(|c| {
        seed_views(c);
        for (cmd, code, msg) in [
            (
                doc! {"create": "bad", "pipeline": []},
                72,
                "'pipeline' requires 'viewOn' to also be specified",
            ),
            (
                doc! {"create": "bad", "viewOn": "src", "pipeline": [{"$nope": 1}]},
                40324,
                "Unrecognized pipeline stage name: '$nope'",
            ),
            (
                doc! {"create": "bad", "viewOn": "src", "pipeline": [{"$merge": {"into": "x"}}]},
                167,
                "Invalid pipeline for view t.bad :: caused by :: The aggregation stage $merge in \
                 location 0 of the pipeline cannot be used in the view definition of t.bad \
                 because it writes to disk",
            ),
            (
                doc! {"create": "bad", "viewOn": "src", "pipeline": [{"$changeStream": {}}]},
                167,
                "Invalid pipeline for view t.bad :: caused by :: $changeStream cannot be used in \
                 a view definition",
            ),
            (
                doc! {"create": "bad", "viewOn": "src", "pipeline": {"$match": {}}},
                14,
                "BSON field 'create.pipeline' is the wrong type 'object', expected type 'array'",
            ),
            (
                doc! {"create": "bad", "viewOn": 5_i32, "pipeline": []},
                14,
                "BSON field 'create.viewOn' is the wrong type 'int', expected type 'string'",
            ),
            (
                doc! {"create": "bad", "viewOn": "", "pipeline": []},
                2,
                "'viewOn' cannot be empty",
            ),
            (
                doc! {"create": "bad", "viewOn": "bad", "pipeline": []},
                5,
                "View cycle detected: t.bad => t.bad => t.bad",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        let listed = dispatch(&doc! {"listCollections": 1, "filter": {"name": "bad"}}, c);
        assert!(
            listed
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .is_empty(),
            "{listed}"
        );
        // A view over a collection that does not exist yet is fine, and empty.
        let reply = dispatch(
            &doc! {"create": "later", "viewOn": "nosuch", "pipeline": []},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
        assert!(view_ids(c, doc! {"find": "later"}).is_empty());
    });
}

// --- validators: every expectation is what mongod 8.2.11 answered (2026-10-09).

#[test]
fn an_ordered_insert_keeps_the_documents_before_a_validation_failure() {
    with_wt(|c| {
        dispatch(&doc! {"create": "q", "validator": {"qty": {"$gte": 0}}}, c);
        let batch = |ids: [i32; 3]| -> Vec<Bson> {
            ids.iter()
                .enumerate()
                .map(|(i, id)| {
                    Bson::Document(doc! {"_id": *id, "qty": if i == 1 { -1 } else { 1 }})
                })
                .collect()
        };
        // The second document fails: the first is in, the third never runs.
        let reply = dispatch(&doc! {"insert": "q", "documents": batch([10, 11, 12])}, c);
        assert_eq!(reply.get_i32("n").unwrap(), 1, "{reply}");
        let errors = reply.get_array("writeErrors").unwrap();
        assert_eq!(errors.len(), 1);
        let e = errors[0].as_document().unwrap();
        assert_eq!(
            (e.get_i32("index").unwrap(), e.get_i32("code").unwrap()),
            (1, 121)
        );
        assert_eq!(
            e.get_document("errInfo")
                .unwrap()
                .get_i32("failingDocumentId")
                .unwrap(),
            11
        );
        // Unordered: both good documents land.
        let reply = dispatch(
            &doc! {"insert": "q", "ordered": false, "documents": batch([20, 21, 22])},
            c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2, "{reply}");
        // A duplicate key before the validation failure stops an ordered
        // batch first, and is the only error reported.
        let reply = dispatch(
            &doc! {"insert": "q", "documents": [{"_id": 10, "qty": 1}, {"_id": 30, "qty": -1}]},
            c,
        );
        let errors = reply.get_array("writeErrors").unwrap();
        assert_eq!(errors.len(), 1, "{reply}");
        assert_eq!(
            errors[0].as_document().unwrap().get_i32("code").unwrap(),
            11000
        );
        assert_eq!(
            view_ids(c, doc! {"find": "q", "sort": {"_id": 1}}),
            vec![10, 20, 22]
        );
    });
}

#[test]
fn validation_options_are_checked_and_stored_as_mongod_stores_them() {
    with_wt(|c| {
        for (cmd, code, msg) in [
            (
                doc! {"create": "x", "validator": {"a": 1}, "validationLevel": "sometimes"},
                2,
                "Enumeration value 'sometimes' for field 'create.validationLevel' is not a valid \
                 value.",
            ),
            (
                doc! {"create": "x", "validator": {"a": 1}, "validationAction": "shout"},
                2,
                "Enumeration value 'shout' for field 'create.validationAction' is not a valid \
                 value.",
            ),
            (
                doc! {"create": "x", "validator": {"$where": "true"}},
                2,
                "$where is not allowed in this context",
            ),
            (
                doc! {"create": "x", "validator": {"$or": [{"a": 1}, {"$text": {"$search": "a"}}]}},
                2,
                "$text is not allowed in this context",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        let near = dispatch(
            &doc! {"create": "x", "validator": {"loc": {"$near": [0, 0]}}},
            c,
        );
        assert_eq!(near.get_i32("code").unwrap(), 5626500, "{near}");

        // An empty validator is no validator.
        dispatch(&doc! {"create": "e", "validator": {}}, c);
        assert_eq!(collection_options(c, "e"), doc! {});
        // `collMod` writes out both the level and the action.
        dispatch(&doc! {"create": "q", "validator": {"a": 1}}, c);
        dispatch(&doc! {"collMod": "q", "validationAction": "warn"}, c);
        assert_eq!(
            collection_options(c, "q"),
            doc! {"validator": {"a": 1}, "validationLevel": "strict", "validationAction": "warn"}
        );
        let bad = dispatch(&doc! {"collMod": "q", "validationLevel": "nope"}, c);
        assert_eq!(
            bad.get_str("errmsg").unwrap(),
            "Enumeration value 'nope' for field 'collMod.validationLevel' is not a valid value."
        );
        // Removing the validator keeps them.
        dispatch(&doc! {"collMod": "q", "validator": {}}, c);
        assert_eq!(
            collection_options(c, "q"),
            doc! {"validationLevel": "strict", "validationAction": "warn"}
        );
        let reply = dispatch(&doc! {"insert": "q", "documents": [{"_id": 1}]}, c);
        assert_eq!(reply.get_i32("n").unwrap(), 1, "{reply}");
    });
}

#[test]
fn out_and_merge_report_a_validation_failure_with_its_details() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "w", "documents": [{"_id": 1, "qty": -1}]},
            c,
        );
        dispatch(&doc! {"create": "m", "validator": {"qty": {"$gte": 0}}}, c);
        for (stage, msg) in [
            (
                doc! {"$out": "m"},
                "Executor error during aggregate command on namespace: t.w :: caused by :: \
                 Document failed validation",
            ),
            (
                doc! {"$merge": {"into": "m"}},
                "Executor error during aggregate command on namespace: t.w :: caused by :: Plan \
                 executor error during update :: caused by :: Document failed validation",
            ),
        ] {
            let reply = dispatch(
                &doc! {"aggregate": "w", "pipeline": [stage.clone()], "cursor": {}},
                c,
            );
            assert_eq!(reply.get_i32("code").unwrap(), 121, "{stage}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{stage}");
            let info = reply.get_document("errInfo").unwrap();
            assert_eq!(info.get_i32("failingDocumentId").unwrap(), 1, "{stage}");
            assert!(info.get_document("details").is_ok(), "{stage}: {reply}");
        }
    });
}

// --- index management: every expectation is what mongod 8.2.11 answered
// --- (2026-10-10).

fn create_index(c: &mut CommandContext, spec: Document) -> Document {
    dispatch(&doc! {"createIndexes": "c", "indexes": [spec]}, c)
}

#[test]
fn drop_indexes_never_drops_the_id_index() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "a": 1, "b": 1}]},
            c,
        );
        for key in ["a", "b", "t"] {
            create_index(c, doc! {"key": {key: 1}, "name": format!("{key}_1")});
        }
        for (cmd, code, msg) in [
            // By key it used to be dropped.
            (
                doc! {"dropIndexes": "c", "index": {"_id": 1}},
                72,
                "cannot drop _id index",
            ),
            (
                doc! {"dropIndexes": "c", "index": "_id_"},
                72,
                "cannot drop _id index",
            ),
            (
                doc! {"dropIndexes": "c", "index": ["a_1", "_id_"]},
                72,
                "cannot drop _id index",
            ),
            (
                doc! {"dropIndexes": "c", "index": ["a_1", "nope"]},
                27,
                "index not found with name [nope]",
            ),
            (
                doc! {"dropIndexes": "c"},
                40414,
                "BSON field 'dropIndexes.index' is missing but a required field",
            ),
            (
                doc! {"dropIndexes": "nosuch", "index": "*"},
                26,
                "ns not found t.nosuch",
            ),
        ] {
            let reply = dispatch(&cmd, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{cmd}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{cmd}");
        }
        // Nothing was dropped by the refused lists.
        assert_eq!(index_names(c, "c"), vec!["_id_", "a_1", "b_1", "t_1"]);
        let reply = dispatch(&doc! {"dropIndexes": "c", "index": ["a_1", "b_1"]}, c);
        assert_eq!(reply.get_i32("nIndexesWas").unwrap(), 4, "{reply}");
        assert_eq!(index_names(c, "c"), vec!["_id_", "t_1"]);
        let reply = dispatch(&doc! {"dropIndexes": "c", "index": "*"}, c);
        assert_eq!(
            reply.get_str("msg").unwrap(),
            "non-_id indexes dropped for collection"
        );
        assert_eq!(index_names(c, "c"), vec!["_id_"]);
    });
}

#[test]
fn create_indexes_refuses_a_bad_spec_before_building_anything() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "a": 1}, {"_id": 2, "a": 1}]},
            c,
        );
        for (spec, code, msg) in [
            (
                doc! {"key": {"z": 0}, "name": "z_0"},
                67,
                "Error in specification { key: { z: 0 }, name: \"z_0\" } :: caused by :: Values \
                 in the index key pattern cannot be 0.",
            ),
            (
                doc! {"key": {"": 1}, "name": "ef"},
                67,
                "Error in specification { key: { : 1 }, name: \"ef\" } :: caused by :: Index \
                 keys cannot be an empty field.",
            ),
            (
                doc! {"key": {"$x": 1}, "name": "dx"},
                67,
                "Error in specification { key: { $x: 1 }, name: \"dx\" } :: caused by :: Index \
                 key contains an illegal field name: field name starts with '$'.",
            ),
            (
                doc! {"key": {"u": 1}, "bogus": true, "name": "u_1"},
                197,
                "Error in specification { key: { u: 1 }, bogus: true, name: \"u_1\" } :: caused \
                 by :: The field 'bogus' is not valid for an index specification. Specification: \
                 { key: { u: 1 }, bogus: true, name: \"u_1\" }",
            ),
            (
                doc! {"key": {"n": 1}, "name": ""},
                67,
                "Error in specification { key: { n: 1 }, name: \"\", v: 2 } :: caused by :: \
                 index name cannot be empty",
            ),
            (
                doc! {"key": {"s": 1}, "name": "*"},
                2,
                "The index name '*' is not valid.",
            ),
            (
                doc! {"key": {"_id": -1}, "name": "_id_-1"},
                2,
                "The field 'key' for an _id index must be {_id: 1}, but got { _id: -1 }",
            ),
            (
                doc! {"key": {"_id": 1}, "name": "_id_", "sparse": true},
                197,
                "The field 'sparse' is not valid for an _id index specification. Specification: \
                 { key: { _id: 1 }, name: \"_id_\", sparse: true, v: 2 }",
            ),
            (
                doc! {"key": {"w": 1}, "expireAfterSeconds": -1, "name": "w_1"},
                67,
                ". Index spec: { key: { w: 1 }, expireAfterSeconds: -1, name: \"w_1\" } :: \
                 caused by :: TTL index 'expireAfterSeconds' option cannot be less than 0",
            ),
            (
                doc! {"key": {"w": 1, "a": 1}, "expireAfterSeconds": 10, "name": "w_1_a_1"},
                67,
                "TTL indexes are single-field indexes, compound indexes do not support TTL. \
                 Index spec: { key: { w: 1, a: 1 }, expireAfterSeconds: 10, name: \"w_1_a_1\" }",
            ),
            (
                doc! {"key": {"p": 1}, "sparse": true, "partialFilterExpression": {"a": 1},
                "name": "p_1"},
                67,
                "Error in specification { key: { p: 1 }, sparse: true, partialFilterExpression: \
                 { a: 1 }, name: \"p_1\", v: 2 } :: caused by :: cannot mix \
                 \"partialFilterExpression\" and \"sparse\" options",
            ),
        ] {
            let reply = create_index(c, spec.clone());
            assert_eq!(reply.get_i32("code").unwrap(), code, "{spec}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{spec}");
        }
        let empty = dispatch(&doc! {"createIndexes": "c", "indexes": []}, c);
        assert_eq!(
            empty.get_str("errmsg").unwrap(),
            "Must specify at least one index to create"
        );
        // A bad spec after a good one builds neither, and creates no collection.
        let reply = dispatch(
            &doc! {"createIndexes": "fresh", "indexes": [
            {"key": {"a": 1}, "name": "a_1"}, {"key": {"z": 0}, "name": "z_0"}]},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 67, "{reply}");
        assert_eq!(
            dispatch(&doc! {"listIndexes": "fresh"}, c)
                .get_i32("code")
                .unwrap(),
            26
        );
        assert_eq!(index_names(c, "c"), vec!["_id_"]);

        // A number is a boolean: `unique: 1` is unique, and these duplicate.
        let reply = create_index(c, doc! {"key": {"a": 1}, "name": "a_1", "unique": 1_i32});
        assert_eq!(reply.get_i32("code").unwrap(), 11000, "{reply}");
        // The `_id` index under another name already exists.
        let reply = create_index(c, doc! {"key": {"_id": 1}, "name": "myid"});
        assert_eq!(
            reply.get_str("note").unwrap(),
            "all indexes already exist",
            "{reply}"
        );
        assert!(!reply.contains_key("createdCollectionAutomatically"));
        // A collection created by the build counts its `_id` index as before.
        let reply = dispatch(
            &doc! {"createIndexes": "fresh", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            c,
        );
        assert_eq!(
            (
                reply.get_i32("numIndexesBefore").unwrap(),
                reply.get_i32("numIndexesAfter").unwrap()
            ),
            (1, 2)
        );
        assert!(reply.get_bool("createdCollectionAutomatically").unwrap());
    });
}

#[test]
fn coll_mod_hides_an_index_and_says_what_changed() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "a": 1}]}, c);
        create_index(c, doc! {"key": {"a": 1}, "name": "a_1"});
        create_index(
            c,
            doc! {"key": {"w": 1}, "name": "w_1", "expireAfterSeconds": 3600_i32},
        );
        let hide = dispatch(
            &doc! {"collMod": "c", "index": {"name": "a_1", "hidden": true}},
            c,
        );
        assert_eq!(
            hide,
            doc! {"hidden_old": false, "hidden_new": true, "ok": 1.0}
        );
        // Already hidden: nothing to report.
        let again = dispatch(
            &doc! {"collMod": "c", "index": {"name": "a_1", "hidden": true}},
            c,
        );
        assert_eq!(again, doc! {"ok": 1.0});
        let show = dispatch(
            &doc! {"collMod": "c", "index": {"keyPattern": {"a": 1}, "hidden": false}},
            c,
        );
        assert_eq!(
            show,
            doc! {"hidden_old": true, "hidden_new": false, "ok": 1.0}
        );
        let ttl = dispatch(
            &doc! {"collMod": "c", "index": {"name": "w_1", "expireAfterSeconds": 60_i32}},
            c,
        );
        assert_eq!(
            ttl,
            doc! {"expireAfterSeconds_old": 3600_i64, "expireAfterSeconds_new": 60_i64, "ok": 1.0}
        );
        let first = dispatch(
            &doc! {"collMod": "c", "index": {"name": "a_1", "expireAfterSeconds": 60_i32}},
            c,
        );
        assert_eq!(first, doc! {"expireAfterSeconds_new": 60_i64, "ok": 1.0});
        for (index, code, msg) in [
            (
                doc! {"name": "_id_", "hidden": true},
                2,
                "can't hide _id index",
            ),
            (
                doc! {"name": "nope", "hidden": true},
                27,
                "cannot find index nope for ns t.c",
            ),
            (
                doc! {"hidden": true},
                72,
                "Must specify either index name or key pattern.",
            ),
            (
                doc! {"name": "a_1"},
                72,
                "no expireAfterSeconds, hidden, unique, or prepareUnique field",
            ),
        ] {
            let reply = dispatch(&doc! {"collMod": "c", "index": index.clone()}, c);
            assert_eq!(reply.get_i32("code").unwrap(), code, "{index}: {reply}");
            assert_eq!(reply.get_str("errmsg").unwrap(), msg, "{index}");
        }
    });
}
