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
            &doc! {"createIndexes": "c", "indexes": [{"key": {"g": "2dsphere", "z": 1}}]},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{r:?}");
        assert!(index_names(c, "c").contains(&"g_2dsphere_z_1".to_string()));
        // A 2d compound index is likewise accepted.
        let r2d = dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"p": "2d", "z": 1}}]},
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
        // re-create ⇒ NamespaceExists
        let reply = dispatch(&doc! {"create": "c"}, c);
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
                {"key": {"b": -1}},  // name auto-derived ⇒ b_-1
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
        assert_eq!(r.get_i32("code").unwrap(), 85);
        assert_eq!(r.get_str("codeName").unwrap(), "IndexOptionsConflict");
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
        let reply = dispatch(&doc! {"renameCollection": "t.a", "to": "t.b"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(index_names(c, "b"), vec!["_id_"]);
    });
}

#[test]
fn rename_nonexistent_source_is_namespace_not_found() {
    // Renaming a missing source is NamespaceNotFound (26), not NamespaceExists
    // (48) — php-lib RenameCollectionFunctionalTest::testRenameNonexistentCollection.
    with_wt(|c| {
        let r = dispatch(&doc! {"renameCollection": "t.nope", "to": "t.dst"}, c);
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 26);
        assert_eq!(r.get_str("codeName").unwrap(), "NamespaceNotFound");
    });
}
