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
        assert_eq!(r.get_i32("expireAfterSeconds_old").unwrap(), 3);
        assert_eq!(r.get_i32("expireAfterSeconds_new").unwrap(), 1000);
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
        assert_eq!(reply.get_i32("numIndexesBefore").unwrap(), 0);
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
