//! A write reads its collection's registry row and index descriptors once per
//! catalog generation (`index_descs_cached`, `coll_options_cached`,
//! `list_indexes`). A stale entry would skip a unique check or an index's
//! entries -- silent corruption -- so every way the catalog changes is pinned
//! here with the cache warm: each test writes first, so the next write would
//! be served from the cache if the change did not move the generation.
//! Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-wcc-{}-{}", std::process::id(), n));
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

/// Insert one document; the write errors it reports (a duplicate key is one).
fn put(st: &Storage, coll: &str, d: Document) -> Vec<Document> {
    st.insert("app", coll, vec![enc(&d)], true).unwrap().1
}

fn dup(errors: &[Document]) -> bool {
    errors.iter().any(|e| e.get_i32("code") == Ok(11000))
}

fn names(st: &Storage, coll: &str) -> Vec<String> {
    st.list_indexes("app", coll)
        .unwrap()
        .iter()
        .map(|d| d.get_str("name").unwrap().to_string())
        .collect()
}

#[test]
fn a_unique_index_created_after_writes_is_enforced_on_the_next() {
    with_db(|st| {
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        assert!(put(st, "c", doc! {"_id": 2, "x": 6}).is_empty());
        assert_eq!(names(st, "c"), vec!["_id_"]);
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
            .unwrap();
        assert_eq!(names(st, "c"), vec!["_id_", "x_1"]);
        assert!(dup(&put(st, "c", doc! {"_id": 3, "x": 5})));
        // And its entries are written: the index finds the new row.
        assert!(put(st, "c", doc! {"_id": 4, "x": 7}).is_empty());
        assert_eq!(
            st.find_matching("app", "c", &doc! {"x": 7}).unwrap().len(),
            1
        );
        // Dropped, it no longer refuses anything.
        assert!(st.drop_index("app", "c", "x_1").unwrap());
        assert_eq!(names(st, "c"), vec!["_id_"]);
        assert!(put(st, "c", doc! {"_id": 5, "x": 5}).is_empty());
    });
}

#[test]
fn a_collection_dropped_and_recreated_has_only_its_new_indexes() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
            .unwrap();
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        assert!(dup(&put(st, "c", doc! {"_id": 2, "x": 5})));
        assert!(st.drop_collection("app", "c").unwrap());
        // Recreated by a write, with no unique index.
        assert!(put(st, "c", doc! {"_id": 1, "x": 5, "y": 10}).is_empty());
        assert!(put(st, "c", doc! {"_id": 2, "x": 5, "y": 20}).is_empty());
        assert_eq!(names(st, "c"), vec!["_id_"]);
        // And a unique index on another field of the new one holds.
        st.create_index("app", "c", "y_1", &doc! {"y": 1}, &doc! {"unique": true})
            .unwrap();
        assert!(put(st, "c", doc! {"_id": 3, "y": 1}).is_empty());
        assert!(dup(&put(st, "c", doc! {"_id": 4, "y": 1})));
    });
}

#[test]
fn a_renamed_collections_indexes_move_with_it() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
            .unwrap();
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        st.rename_collection("app", "c", "app", "d", false).unwrap();
        // The new name enforces the index; the old name is a new collection.
        assert!(dup(&put(st, "d", doc! {"_id": 2, "x": 5})));
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        assert!(put(st, "c", doc! {"_id": 2, "x": 5}).is_empty());
        assert_eq!(names(st, "c"), vec!["_id_"]);
    });
}

#[test]
fn an_index_a_transaction_creates_binds_its_own_writes_and_others_after_commit() {
    with_db(|st| {
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
                .unwrap();
            // Its own write sees the index it created.
            assert!(dup(&put(st, "c", doc! {"_id": 2, "x": 5})));
        })
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        // Everyone's write sees it once committed.
        assert!(dup(&put(st, "c", doc! {"_id": 3, "x": 5})));
        assert_eq!(names(st, "c"), vec!["_id_", "x_1"]);
    });
}

#[test]
fn an_index_a_rolled_back_transaction_created_binds_nothing() {
    with_db(|st| {
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
                .unwrap();
            assert_eq!(names(st, "c"), vec!["_id_", "x_1"]);
        })
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
        assert_eq!(names(st, "c"), vec!["_id_"]);
        assert!(put(st, "c", doc! {"_id": 2, "x": 5}).is_empty());
    });
}

#[test]
fn a_write_in_a_transaction_after_anothers_index_commit_sees_it() {
    with_db(|st| {
        assert!(put(st, "c", doc! {"_id": 1, "x": 5}).is_empty());
        // A transaction warms the cache in its own snapshot...
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            assert!(put(st, "c", doc! {"_id": 2, "x": 6}).is_empty());
        })
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        // ...another creates a unique index...
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {"unique": true})
            .unwrap();
        // ...and a transaction begun after it is bound by it.
        let mut later = st.begin_user_transaction().unwrap();
        let errors = st
            .with_user_transaction(&mut later, || put(st, "c", doc! {"_id": 3, "x": 6}))
            .unwrap();
        st.rollback_user_transaction(&mut later).unwrap();
        assert!(dup(&errors));
    });
}

#[test]
fn a_capped_collection_replacing_a_plain_one_is_bounded() {
    with_db(|st| {
        assert!(put(st, "c", doc! {"_id": 1}).is_empty());
        assert!(put(st, "c", doc! {"_id": 2}).is_empty());
        assert!(st.drop_collection("app", "c").unwrap());
        st.create_collection_with_options(
            "app",
            "c",
            &doc! {"capped": true, "size": 4096, "max": 2},
        )
        .unwrap();
        for i in 0..5 {
            assert!(put(st, "c", doc! {"_id": i}).is_empty());
        }
        assert_eq!(st.find_matching("app", "c", &doc! {}).unwrap().len(), 2);
    });
}
