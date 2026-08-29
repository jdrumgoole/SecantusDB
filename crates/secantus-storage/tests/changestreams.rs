//! Change-stream projection tests (Phase 4 sub-phase 3e): oplog entries → change
//! events — operationType mapping (insert/update/replace/delete/drop),
//! `fullDocument` (+ updateLookup), `fullDocumentBeforeChange`, scope filtering,
//! resume tokens, invalidate, and the split envelope. Storage-backed lookups run
//! against real WiredTiger; pure projection uses synthetic oplog entries.

use bson::{doc, Bson, Document, Timestamp};
use secantus_storage::changestreams::{
    self, Scope, FULL_DOC_DEFAULT, FULL_DOC_REQUIRED, FULL_DOC_UPDATE_LOOKUP,
};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-cs-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

/// Project the oplog entry with the given `op` (storage-emitted), at `scope`.
fn project_op(
    st: &Storage,
    op: &str,
    full_doc: &str,
    before: &str,
    scope: &Scope,
) -> (Option<Document>, bool) {
    // Async lane: wait out the drainer so the emitted entry is readable
    // (no-op in sync mode).
    st.flush_oplog();
    let (seq, blob) = st
        .read_oplog(1, 100)
        .unwrap()
        .into_iter()
        .find(|(_, b)| decode(b).get_str("op").unwrap() == op)
        .expect("oplog entry");
    changestreams::project(seq, &decode(&blob), st, full_doc, before, scope, false).unwrap()
}

fn coll_scope() -> Scope {
    Scope::Coll {
        db: "app".into(),
        coll: "c".into(),
    }
}

#[test]
fn insert_event_carries_full_document() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 7}))
            .unwrap();
        let (ev, inv) = project_op(st, "i", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &coll_scope());
        let ev = ev.unwrap();
        assert!(!inv);
        assert_eq!(ev.get_str("operationType").unwrap(), "insert");
        assert_eq!(
            ev.get_document("ns").unwrap(),
            &doc! {"db": "app", "coll": "c"}
        );
        assert_eq!(
            ev.get_document("documentKey")
                .unwrap()
                .get_i32("_id")
                .unwrap(),
            1
        );
        assert_eq!(
            ev.get_document("fullDocument")
                .unwrap()
                .get_i32("x")
                .unwrap(),
            7
        );
        // _id is an opaque resume token, clusterTime is the entry's ts.
        assert!(ev.get_document("_id").unwrap().contains_key("_data"));
        assert!(matches!(ev.get("clusterTime"), Some(Bson::Timestamp(_))));
    });
}

#[test]
fn replace_is_replace_op_with_full_document() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 1}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"x": 2}))
            .unwrap();
        let (ev, _) = project_op(st, "u", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &coll_scope());
        let ev = ev.unwrap();
        // No $v/diff in `o` -> the change stream calls it a replace.
        assert_eq!(ev.get_str("operationType").unwrap(), "replace");
        assert_eq!(
            ev.get_document("fullDocument")
                .unwrap()
                .get_i32("x")
                .unwrap(),
            2
        );
        assert!(!ev.contains_key("updateDescription"));
    });
}

#[test]
fn delete_event_has_document_key_only() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 9})).unwrap();
        st.delete_by_id("app", "c", &Bson::Int32(9)).unwrap();
        let (ev, _) = project_op(st, "d", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &coll_scope());
        let ev = ev.unwrap();
        assert_eq!(ev.get_str("operationType").unwrap(), "delete");
        assert_eq!(
            ev.get_document("documentKey")
                .unwrap()
                .get_i32("_id")
                .unwrap(),
            9
        );
        assert!(!ev.contains_key("fullDocument"));
    });
}

#[test]
fn update_diff_event_has_update_description() {
    with_db(|st| {
        // Synthetic operator-style update entry (the storage layer emits
        // replacements; a diff-shaped `o` is what an operator update would log).
        let entry = doc! {
            "op": "u",
            "ns": "app.c",
            "o": {"$v": 2i32, "diff": {"u": {"x": 2}}},
            "o2": {"_id": 1},
            "ts": Bson::Timestamp(Timestamp { time: 100, increment: 1 }),
        };
        let (ev, inv) = changestreams::project(
            5,
            &entry,
            st,
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &Scope::Cluster,
            false,
        )
        .unwrap();
        let ev = ev.unwrap();
        assert!(!inv);
        assert_eq!(ev.get_str("operationType").unwrap(), "update");
        assert_eq!(
            ev.get_document("updateDescription").unwrap(),
            &doc! {"u": {"x": 2}}
        );
    });
}

#[test]
fn update_lookup_fetches_current_document() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 5}))
            .unwrap();
        let entry = doc! {
            "op": "u",
            "ns": "app.c",
            "o": {"$v": 2i32, "diff": {"u": {"x": 5}}},
            "o2": {"_id": 1},
            "ts": Bson::Timestamp(Timestamp { time: 100, increment: 1 }),
        };
        let (ev, _) = changestreams::project(
            5,
            &entry,
            st,
            FULL_DOC_UPDATE_LOOKUP,
            FULL_DOC_DEFAULT,
            &Scope::Cluster,
            false,
        )
        .unwrap();
        let ev = ev.unwrap();
        // updateLookup re-fetches the current doc from storage.
        assert_eq!(
            ev.get_document("fullDocument")
                .unwrap()
                .get_i32("x")
                .unwrap(),
            5
        );
    });
}

#[test]
fn full_document_before_change_from_preimage() {
    with_db(|st| {
        st.set_collection_options(
            "app",
            "c",
            &doc! {"changeStreamPreAndPostImages": {"enabled": true}},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "old"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "new"}))
            .unwrap();
        let (ev, _) = project_op(st, "u", FULL_DOC_DEFAULT, FULL_DOC_REQUIRED, &coll_scope());
        let ev = ev.unwrap();
        assert_eq!(
            ev.get_document("fullDocumentBeforeChange")
                .unwrap()
                .get_str("v")
                .unwrap(),
            "old"
        );
    });
}

#[test]
fn scope_filters_events() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        // Cluster + matching db + matching coll all surface it.
        assert!(
            project_op(st, "i", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &Scope::Cluster)
                .0
                .is_some()
        );
        assert!(project_op(
            st,
            "i",
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &Scope::Db("app".into())
        )
        .0
        .is_some());
        assert!(
            project_op(st, "i", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &coll_scope())
                .0
                .is_some()
        );
        // A different db / collection does not.
        assert!(project_op(
            st,
            "i",
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &Scope::Db("other".into())
        )
        .0
        .is_none());
        let other_coll = Scope::Coll {
            db: "app".into(),
            coll: "c2".into(),
        };
        assert!(
            project_op(st, "i", FULL_DOC_DEFAULT, FULL_DOC_DEFAULT, &other_coll)
                .0
                .is_none()
        );
    });
}

#[test]
fn rename_event_carries_operation_description_with_drop_target() {
    // With showExpandedEvents, a rename that replaced an existing target carries
    // `operationDescription: {to, dropTarget}` (the dropTarget UUID comes from
    // the oplog `o.dropTarget`). Mirrors mongod 6.0+ expanded rename events.
    with_db(|st| {
        let dropped_ui = Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid,
            bytes: vec![7u8; 16],
        });
        let entry = doc! {
            "op": "c",
            "ns": "app.$cmd",
            "o": {
                "renameCollection": "app.c",
                "to": "app.foo",
                "dropTarget": dropped_ui.clone(),
            },
            "ts": Bson::Timestamp(Timestamp { time: 300, increment: 1 }),
        };
        let (ev, inv) = changestreams::project(
            7,
            &entry,
            st,
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &coll_scope(),
            true, // show_expanded_events
        )
        .unwrap();
        let ev = ev.unwrap();
        assert_eq!(ev.get_str("operationType").unwrap(), "rename");
        assert_eq!(
            ev.get_document("to").unwrap(),
            &doc! {"db": "app", "coll": "foo"}
        );
        let op_desc = ev.get_document("operationDescription").unwrap();
        assert_eq!(
            op_desc.get_document("to").unwrap(),
            &doc! {"db": "app", "coll": "foo"}
        );
        assert_eq!(op_desc.get("dropTarget"), Some(&dropped_ui));
        assert!(inv); // a rename of the watched collection ends the stream

        // Without showExpandedEvents, there's no operationDescription at all.
        let (ev2, _) = changestreams::project(
            7,
            &entry,
            st,
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &coll_scope(),
            false,
        )
        .unwrap();
        assert!(ev2.unwrap().get("operationDescription").is_none());
    });
}

#[test]
fn noop_heartbeat_projects_nothing() {
    with_db(|st| {
        let seq = st.emit_noop_heartbeat().unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(1, 10).unwrap();
        let entry = decode(&rows[0].1);
        let (ev, inv) = changestreams::project(
            seq,
            &entry,
            st,
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &Scope::Cluster,
            false,
        )
        .unwrap();
        assert!(ev.is_none());
        assert!(!inv);
    });
}

#[test]
fn drop_event_invalidates_collection_scope() {
    with_db(|st| {
        let entry = doc! {
            "op": "c",
            "ns": "app.$cmd",
            "o": {"drop": "c"},
            "ts": Bson::Timestamp(Timestamp { time: 200, increment: 1 }),
        };
        let (ev, inv) = changestreams::project(
            7,
            &entry,
            st,
            FULL_DOC_DEFAULT,
            FULL_DOC_DEFAULT,
            &coll_scope(),
            false,
        )
        .unwrap();
        let ev = ev.unwrap();
        assert_eq!(ev.get_str("operationType").unwrap(), "drop");
        assert_eq!(
            ev.get_document("ns").unwrap(),
            &doc! {"db": "app", "coll": "c"}
        );
        assert!(inv); // a drop on the watched collection ends the stream
        let invalidate = changestreams::invalidate_event(7, &entry).unwrap();
        assert_eq!(invalidate.get_str("operationType").unwrap(), "invalidate");
    });
}

#[test]
fn resume_token_round_trips() {
    let data = changestreams::ResumeTokenData {
        seq: 42,
        ts: Timestamp {
            time: 123,
            increment: 4,
        },
        ns: "app.c".into(),
        document_key: doc! {"_id": 7},
        from_invalidate: false,
    };
    let token = changestreams::make_resume_token(&data).unwrap();
    let parsed = changestreams::parse_resume_token(&token).unwrap();
    assert_eq!(parsed, data);
}

#[test]
fn small_event_gets_single_fragment() {
    let event = doc! {"operationType": "insert", "x": 1};
    let frags = changestreams::stamp_split_event(event).unwrap();
    assert_eq!(frags.len(), 1);
    assert_eq!(
        frags[0].get_document("splitEvent").unwrap(),
        &doc! {"fragment": 1, "of": 1}
    );
}

#[test]
fn over_16mb_event_splits_by_heavy_field() {
    // An update with a ~10MB pre-image and a ~10MB updated value exceeds 16MB and
    // has two heavy (>1MB) fields, so it splits into 2 fragments — one heavy field
    // each, light metadata copied into both (mirrors pymongo test_split_large_change).
    let big = "q".repeat(10 * 1024 * 1024);
    let event = doc! {
        "_id": doc! {"_data": "tok"},
        "operationType": "update",
        "ns": doc! {"db": "d", "coll": "c"},
        "fullDocumentBeforeChange": big.clone(),
        "updateDescription": doc! {"updatedFields": doc! {"value": big}},
    };
    let frags = changestreams::stamp_split_event(event).unwrap();
    assert_eq!(frags.len(), 2);
    assert_eq!(
        frags[0].get_document("splitEvent").unwrap(),
        &doc! {"fragment": 1, "of": 2}
    );
    assert_eq!(
        frags[1].get_document("splitEvent").unwrap(),
        &doc! {"fragment": 2, "of": 2}
    );
    // Light metadata is copied verbatim into every fragment.
    for f in &frags {
        assert_eq!(f.get_str("operationType").unwrap(), "update");
        assert!(f.get_document("ns").is_ok());
    }
    // Each heavy field lands in exactly one fragment.
    let has_pre: Vec<bool> = frags
        .iter()
        .map(|f| f.contains_key("fullDocumentBeforeChange"))
        .collect();
    assert_eq!(has_pre.iter().filter(|b| **b).count(), 1);
}
