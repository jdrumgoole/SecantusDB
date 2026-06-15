//! Write-path tests (Phase 4 sub-phase 5, slice 5a): `update_matching`,
//! `delete_matching`, and `count_matching` in the Rust storage engine —
//! operator + replacement updates, multi / single, upsert, unique enforcement,
//! index-entry upkeep, and the `$v:2` diff vs full-doc oplog emission. Against
//! real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, UpdateOutcome};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-write-{}-{}", std::process::id(), n));
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

fn get_doc(st: &Storage, id: i32) -> Document {
    let blob = st
        .find_by_id("app", "c", &Bson::Int32(id))
        .unwrap()
        .expect("doc present");
    decode(&blob)
}

fn seed(st: &Storage, docs: &[Document]) {
    for d in docs {
        st.insert_one("app", "c", &enc(d)).unwrap();
    }
}

#[test]
fn update_operator_single_match() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 1, "x": 1}, doc! {"_id": 2, "x": 1}]);
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"x": 1},
                &doc! {"$set": {"y": 9}},
                false,
                false,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            out,
            UpdateOutcome {
                matched: 1,
                modified: 1,
                upserted_id: None,
            }
        );
        // Exactly one of the two got y=9 (single, not multi).
        let with_y = (1..=2)
            .filter(|i| get_doc(st, *i).contains_key("y"))
            .count();
        assert_eq!(with_y, 1);
    });
}

#[test]
fn update_operator_multi() {
    with_db(|st| {
        seed(
            st,
            &[
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 1},
                doc! {"_id": 3, "x": 2},
            ],
        );
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"x": 1},
                &doc! {"$inc": {"x": 10}},
                true,
                false,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(out.matched, 2);
        assert_eq!(out.modified, 2);
        assert_eq!(get_doc(st, 1).get_i32("x").unwrap(), 11);
        assert_eq!(get_doc(st, 2).get_i32("x").unwrap(), 11);
        assert_eq!(get_doc(st, 3).get_i32("x").unwrap(), 2);
    });
}

#[test]
fn update_no_op_when_unchanged() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 1, "x": 5}]);
        // Setting x to its existing value matches but doesn't modify.
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"x": 5},
                &doc! {"$set": {"x": 5}},
                false,
                false,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(out.matched, 1);
        assert_eq!(out.modified, 0);
    });
}

#[test]
fn update_replacement_preserves_id() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 1, "x": 1, "y": 2}]);
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"_id": 1},
                &doc! {"z": 99},
                false,
                false,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(out.modified, 1);
        let d = get_doc(st, 1);
        assert_eq!(d.get_i32("_id").unwrap(), 1);
        assert_eq!(d.get_i32("z").unwrap(), 99);
        assert!(!d.contains_key("x")); // replacement dropped the old fields
    });
}

#[test]
fn update_upsert_inserts_when_no_match() {
    with_db(|st| {
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"k": "abc"},
                &doc! {"$set": {"n": 1}},
                false,
                true,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(out.matched, 0);
        assert_eq!(out.modified, 0);
        assert!(out.upserted_id.is_some());
        // The upserted doc carries the filter's bare-equality seed + the update.
        let all = st.find_matching("app", "c", &doc! {"k": "abc"}).unwrap();
        assert_eq!(all.len(), 1);
        let d = decode(&all[0]);
        assert_eq!(d.get_str("k").unwrap(), "abc");
        assert_eq!(d.get_i32("n").unwrap(), 1);
    });
}

#[test]
fn update_maintains_index_entries() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        seed(st, &[doc! {"_id": 1, "x": 10}]);
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"x": 20}},
            false,
            false,
            &[],
            &bson::Document::new(),
            None,
            None,
        )
        .unwrap();
        // The index now finds the doc at its new value, not the old one.
        assert_eq!(
            st.find_matching("app", "c", &doc! {"x": 20}).unwrap().len(),
            1
        );
        assert_eq!(
            st.find_matching("app", "c", &doc! {"x": 10}).unwrap().len(),
            0
        );
    });
}

#[test]
fn update_rejects_unique_conflict() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
            .unwrap();
        seed(st, &[doc! {"_id": 1, "x": 1}, doc! {"_id": 2, "x": 2}]);
        // Moving _id:2 onto x:1 collides with _id:1.
        let err = st.update_matching(
            "app",
            "c",
            &doc! {"_id": 2},
            &doc! {"$set": {"x": 1}},
            false,
            false,
            &[],
            &bson::Document::new(),
            None,
            None,
        );
        assert!(err.is_err());
        // The collision left _id:2 unchanged.
        assert_eq!(get_doc(st, 2).get_i32("x").unwrap(), 2);
    });
}

#[test]
fn update_operator_emits_v2_diff_oplog() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 1, "x": 1}]);
        let floor = st.oplog_tail_seq();
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"y": 7}},
            false,
            false,
            &[],
            &bson::Document::new(),
            None,
            None,
        )
        .unwrap();
        let rows = st.read_oplog(floor + 1, 100).unwrap();
        assert_eq!(rows.len(), 1);
        let e = decode(&rows[0].1);
        assert_eq!(e.get_str("op").unwrap(), "u");
        let o = e.get_document("o").unwrap();
        assert_eq!(o.get_i32("$v").unwrap(), 2);
        let diff = o.get_document("diff").unwrap();
        assert_eq!(
            diff.get_document("updatedFields")
                .unwrap()
                .get_i32("y")
                .unwrap(),
            7
        );
        assert_eq!(e.get_document("o2").unwrap().get_i32("_id").unwrap(), 1);
    });
}

#[test]
fn update_replacement_emits_full_doc_oplog() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 1, "x": 1}]);
        let floor = st.oplog_tail_seq();
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"z": 5},
            false,
            false,
            &[],
            &bson::Document::new(),
            None,
            None,
        )
        .unwrap();
        let rows = st.read_oplog(floor + 1, 100).unwrap();
        let e = decode(&rows[0].1);
        assert_eq!(e.get_str("op").unwrap(), "u");
        let o = e.get_document("o").unwrap();
        // Full replacement doc, not a $v:2 diff envelope.
        assert!(!o.contains_key("$v"));
        assert_eq!(o.get_i32("z").unwrap(), 5);
        assert_eq!(o.get_i32("_id").unwrap(), 1);
    });
}

#[test]
fn delete_matching_single_and_multi() {
    with_db(|st| {
        seed(
            st,
            &[
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 1},
                doc! {"_id": 3, "x": 2},
            ],
        );
        // limit=1 deletes one of the two x:1 docs.
        assert_eq!(
            st.delete_matching("app", "c", &doc! {"x": 1}, 1, &bson::Document::new(), None)
                .unwrap(),
            1
        );
        assert_eq!(
            st.count_matching("app", "c", &doc! {"x": 1}, None).unwrap(),
            1
        );
        // limit=0 deletes the rest of the x:1 docs.
        assert_eq!(
            st.delete_matching("app", "c", &doc! {"x": 1}, 0, &bson::Document::new(), None)
                .unwrap(),
            1
        );
        assert_eq!(
            st.count_matching("app", "c", &doc! {"x": 1}, None).unwrap(),
            0
        );
        // The x:2 doc is untouched.
        assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 1);
    });
}

#[test]
fn delete_maintains_index_entries() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        seed(st, &[doc! {"_id": 1, "x": 7}]);
        assert_eq!(
            st.find_matching("app", "c", &doc! {"x": 7}).unwrap().len(),
            1
        );
        assert_eq!(
            st.delete_matching("app", "c", &doc! {"x": 7}, 0, &bson::Document::new(), None)
                .unwrap(),
            1
        );
        // No stale index entry survives the delete.
        assert_eq!(
            st.find_matching("app", "c", &doc! {"x": 7}).unwrap().len(),
            0
        );
        assert_eq!(st.index_entries("app", "c", "x_1").unwrap().len(), 0);
    });
}

#[test]
fn delete_emits_oplog_delete_entry() {
    with_db(|st| {
        seed(st, &[doc! {"_id": 42, "x": 1}]);
        let floor = st.oplog_tail_seq();
        st.delete_matching(
            "app",
            "c",
            &doc! {"_id": 42},
            0,
            &bson::Document::new(),
            None,
        )
        .unwrap();
        let rows = st.read_oplog(floor + 1, 100).unwrap();
        assert_eq!(rows.len(), 1);
        let e = decode(&rows[0].1);
        assert_eq!(e.get_str("op").unwrap(), "d");
        assert_eq!(e.get_document("o").unwrap().get_i32("_id").unwrap(), 42);
        assert_eq!(e.get_document("o2").unwrap().get_i32("_id").unwrap(), 42);
    });
}

#[test]
fn count_matching_empty_filter_and_predicate() {
    with_db(|st| {
        seed(
            st,
            &[
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 2},
                doc! {"_id": 3, "x": 2},
            ],
        );
        assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 3);
        assert_eq!(
            st.count_matching("app", "c", &doc! {"x": 2}, None).unwrap(),
            2
        );
        assert_eq!(
            st.count_matching("app", "c", &doc! {"x": {"$gt": 1}}, None)
                .unwrap(),
            2
        );
    });
}

#[test]
fn update_skips_oplog_when_disabled() {
    // Oplog-disabled path: a fresh Storage with oplog off does no oplog writes.
    let home = temp_home();
    {
        let mut st = Storage::open(home.to_str().unwrap()).unwrap();
        st.set_enable_oplog(false);
        seed(&st, &[doc! {"_id": 1, "x": 1}]);
        let out = st
            .update_matching(
                "app",
                "c",
                &doc! {"_id": 1},
                &doc! {"$set": {"x": 2}},
                false,
                false,
                &[],
                &bson::Document::new(),
                None,
                None,
            )
            .unwrap();
        assert_eq!(out.modified, 1);
        assert_eq!(get_doc(&st, 1).get_i32("x").unwrap(), 2);
        assert_eq!(st.read_oplog(1, 100).unwrap().len(), 0);
    }
    let _ = std::fs::remove_dir_all(&home);
}
