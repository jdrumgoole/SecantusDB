//! Index integration tests (Phase 4 sub-phase 2, slice 2a): the registry +
//! create/list/drop + index-entry maintenance on CRUD, against real WiredTiger.
//! Byte-exact key-layout assertions live in the crate's unit tests; these cover
//! the storage-backed behaviour (entries actually written/retracted, registry
//! shape, error paths).

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-idx-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st); // final checkpoint before the files disappear
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn index_names(st: &Storage, db: &str, coll: &str) -> Vec<String> {
    st.list_indexes(db, coll)
        .unwrap()
        .iter()
        .map(|d| d.get_str("name").unwrap().to_string())
        .collect()
}

/// Whether index `name` carries the `multikey: true` flag (absent == false).
fn multikey_flag(st: &Storage, db: &str, coll: &str, name: &str) -> bool {
    st.list_indexes(db, coll)
        .unwrap()
        .iter()
        .find(|d| d.get_str("name").unwrap() == name)
        .map(|d| d.get_bool("multikey").unwrap_or(false))
        .unwrap_or(false)
}

#[test]
fn create_list_drop_roundtrip() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 5}))
            .unwrap();
        assert!(st
            .create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap());
        // _id_ (virtual) first, then a_1, sorted by name.
        assert_eq!(index_names(st, "app", "c"), vec!["_id_", "a_1"]);
        // Re-creating with the same options is a no-op (false).
        assert!(!st
            .create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap());
        assert!(st.drop_index("app", "c", "a_1").unwrap());
        assert_eq!(index_names(st, "app", "c"), vec!["_id_"]);
        // Dropping a missing index returns false; _id_ can't be dropped.
        assert!(!st.drop_index("app", "c", "a_1").unwrap());
        assert!(!st.drop_index("app", "c", "_id_").unwrap());
    });
}

#[test]
fn list_indexes_empty_for_missing_collection() {
    with_db(|st| {
        assert!(st.list_indexes("app", "nope").unwrap().is_empty());
    });
}

#[test]
fn create_index_on_existing_data_populates_entries() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": i}))
                .unwrap();
        }
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 3);
    });
}

#[test]
fn entries_maintained_on_insert_update_delete() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 10}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 20}))
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 2);

        // Replace doc 1's indexed value: still two docs, so still two entries
        // (the old a:10 entry is retracted, a:99 written).
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"a": 99}))
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 2);

        st.delete_by_id("app", "c", &Bson::Int32(2)).unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 1);
    });
}

#[test]
fn multikey_array_writes_per_element_entries() {
    with_db(|st| {
        st.create_index("app", "c", "tags_1", &doc! {"tags": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "tags": ["py", "go"]}))
            .unwrap();
        // Two per-element entries + one whole-array entry — the multikey layout.
        // (Lazily flagging the index `multikey` on *insert* is slice 2d; here we
        // only assert the entry variants are written. Create-time detection is
        // covered by `create_index_detects_multikey_on_existing_data`.)
        assert_eq!(st.index_entries("app", "c", "tags_1").unwrap().len(), 3);
    });
}

#[test]
fn insert_array_after_create_lazily_marks_multikey() {
    with_db(|st| {
        st.create_index("app", "c", "tags_1", &doc! {"tags": 1}, &doc! {})
            .unwrap();
        // Created over an empty collection -> not multikey yet.
        assert!(!multikey_flag(st, "app", "c", "tags_1"));
        // Inserting an array-valued doc lazily flags the index.
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "tags": ["a", "b"]}))
            .unwrap();
        assert!(multikey_flag(st, "app", "c", "tags_1"));
    });
}

#[test]
fn replace_introducing_array_marks_multikey() {
    with_db(|st| {
        st.create_index("app", "c", "t_1", &doc! {"t": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": 5}))
            .unwrap();
        assert!(!multikey_flag(st, "app", "c", "t_1"));
        // Replacing the doc's scalar value with an array flags the index.
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"t": [1, 2]}))
            .unwrap();
        assert!(multikey_flag(st, "app", "c", "t_1"));
    });
}

#[test]
fn multikey_flag_is_sticky() {
    with_db(|st| {
        st.create_index("app", "c", "tags_1", &doc! {"tags": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "tags": ["a"]}))
            .unwrap();
        assert!(multikey_flag(st, "app", "c", "tags_1"));
        // Deleting the only array doc does NOT clear the flag.
        st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap();
        assert!(multikey_flag(st, "app", "c", "tags_1"));
        // A later scalar insert leaves it flagged too.
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "tags": "x"}))
            .unwrap();
        assert!(multikey_flag(st, "app", "c", "tags_1"));
    });
}

#[test]
fn create_index_detects_multikey_on_existing_data() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "tags": ["a", "b"]}))
            .unwrap();
        st.create_index("app", "c", "tags_1", &doc! {"tags": 1}, &doc! {})
            .unwrap();
        let idx = st.list_indexes("app", "c").unwrap();
        let tags = idx
            .iter()
            .find(|d| d.get_str("name").unwrap() == "tags_1")
            .unwrap();
        assert!(tags.get_bool("multikey").unwrap());
    });
}

#[test]
fn compound_index_entries() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1, "b": 2}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 1, "b": 3}))
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1_b_1").unwrap().len(), 2);
    });
}

#[test]
fn reject_text_and_hashed_indexes() {
    with_db(|st| {
        assert!(matches!(
            st.create_index("app", "c", "t_text", &doc! {"t": "text"}, &doc! {})
                .unwrap_err(),
            StorageError::CreateIndexUnsupported(_)
        ));
        assert!(matches!(
            st.create_index("app", "c", "h_hashed", &doc! {"h": "hashed"}, &doc! {})
                .unwrap_err(),
            StorageError::CreateIndexUnsupported(_)
        ));
    });
}

#[test]
fn conflicting_options_error() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {"unique": true})
            .unwrap();
        // Re-create with a different `unique` value -> conflict.
        assert!(matches!(
            st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
                .unwrap_err(),
            StorageError::IndexOptionsConflict(_)
        ));
    });
}

#[test]
fn drop_index_removes_entries() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 5}))
            .unwrap();
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 1);
        st.drop_index("app", "c", "a_1").unwrap();
        assert_eq!(st.index_entries("app", "c", "a_1").unwrap().len(), 0);
    });
}

#[test]
fn drop_all_indexes_clears_registry() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        st.create_index("app", "c", "b_1", &doc! {"b": 1}, &doc! {})
            .unwrap();
        assert_eq!(st.drop_all_indexes("app", "c").unwrap(), 2);
        assert_eq!(index_names(st, "app", "c"), vec!["_id_"]);
    });
}
