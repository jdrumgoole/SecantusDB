//! Unique / sparse / partial index write-path tests (Phase 4 sub-phase 2, slice
//! 2e-1): unique enforcement on insert / replace / create-over-existing-data,
//! sparse entry-gating, and partial-index entry-gating, against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-uniq-{}-{}", std::process::id(), n));
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

fn entry_count(st: &Storage, db: &str, coll: &str, name: &str) -> usize {
    st.index_entries(db, coll, name).unwrap().len()
}

#[test]
fn unique_rejects_duplicate_insert() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": "x"}))
            .unwrap();
        match st.insert_one("app", "c", &enc(&doc! {"_id": 2, "e": "x"})) {
            Err(StorageError::DuplicateKey(c)) => {
                assert_eq!(c.index, "e_1");
                assert_eq!(c.key_pattern, doc! {"e": 1});
                assert_eq!(c.key_value, doc! {"e": "x"});
            }
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
        // A distinct value is fine.
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "e": "y"}))
            .unwrap();
    });
}

#[test]
fn unique_create_over_existing_dupes_errors() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": "x"}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "e": "x"}))
            .unwrap();
        let err = st
            .create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap_err();
        assert!(matches!(err, StorageError::DuplicateKey(_)));
        // Distinct existing data builds fine.
    });
}

#[test]
fn unique_create_over_distinct_data_ok() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": "x"}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "e": "y"}))
            .unwrap();
        assert!(st
            .create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap());
        assert_eq!(entry_count(st, "app", "c", "e_1"), 2);
    });
}

#[test]
fn unique_replace_into_existing_value_rejected_self_ok() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": "a"}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "e": "b"}))
            .unwrap();
        // Replacing doc 2's value with "a" collides with doc 1.
        let err = st
            .replace_by_id("app", "c", &Bson::Int32(2), &enc(&doc! {"e": "a"}))
            .unwrap_err();
        assert!(matches!(err, StorageError::DuplicateKey(_)));
        // Replacing doc 1 while keeping its own "a" is fine (own row excluded).
        assert!(st
            .replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"e": "a", "x": 1}))
            .unwrap());
        // ...and changing doc 1 to a fresh value is fine.
        assert!(st
            .replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"e": "c"}))
            .unwrap());
    });
}

#[test]
fn sparse_index_skips_missing_field_docs() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"sparse": true})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": 5}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap(); // no `e`
        assert_eq!(entry_count(st, "app", "c", "e_1"), 1);
    });
}

#[test]
fn unique_sparse_allows_multiple_missing() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "e_1",
            &doc! {"e": 1},
            &doc! {"unique": true, "sparse": true},
        )
        .unwrap();
        // Two docs missing `e` don't collide (no canonical key under sparse).
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "e": 7}))
            .unwrap();
        // But two present-and-equal values still collide.
        let err = st
            .insert_one("app", "c", &enc(&doc! {"_id": 4, "e": 7}))
            .unwrap_err();
        assert!(matches!(err, StorageError::DuplicateKey(_)));
    });
}

#[test]
fn partial_index_only_indexes_matching_docs() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "n_1",
            &doc! {"n": 1},
            &doc! {"partialFilterExpression": {"status": "active"}},
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 1, "n": 1, "status": "active"}),
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "n": 2, "status": "inactive"}),
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "n": 3}))
            .unwrap(); // no status
                       // Only the active doc is indexed.
        assert_eq!(entry_count(st, "app", "c", "n_1"), 1);
    });
}

#[test]
fn unique_partial_scopes_conflict_to_matching_docs() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "n_1",
            &doc! {"n": 1},
            &doc! {"unique": true, "partialFilterExpression": {"status": "active"}},
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 1, "n": 5, "status": "active"}),
        )
        .unwrap();
        // Same n but not in the partial set -> no conflict.
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "n": 5, "status": "inactive"}),
        )
        .unwrap();
        // Same n and in the partial set -> conflict.
        let err = st
            .insert_one(
                "app",
                "c",
                &enc(&doc! {"_id": 3, "n": 5, "status": "active"}),
            )
            .unwrap_err();
        assert!(matches!(err, StorageError::DuplicateKey(_)));
    });
}

#[test]
fn delete_then_reinsert_same_unique_value_ok() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "e": "x"}))
            .unwrap();
        assert!(st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap());
        // The value frees up once its doc (and entry) is gone.
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "e": "x"}))
            .unwrap();
        assert_eq!(entry_count(st, "app", "c", "e_1"), 1);
    });
}
