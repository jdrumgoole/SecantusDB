//! DDL statement-transaction tests: create/drop index and create/drop/rename
//! collection run their row writes (registry, index entries, oplog) inside one
//! WT transaction, so they roll back atomically — pinned here through the
//! user-transaction machinery (the same `with_statement_txn` path gives the
//! autocommit case its crash atomicity, which only a crash could observe
//! directly). Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-ddltxn-{}-{}", std::process::id(), n));
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

fn index_names(st: &Storage, db: &str, coll: &str) -> Vec<String> {
    st.list_indexes(db, coll)
        .unwrap()
        .iter()
        .map(|d| d.get_str("name").unwrap().to_string())
        .collect()
}

#[test]
fn drop_index_rolls_back_inside_user_transaction() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 5}))
            .unwrap();
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &Document::new())
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let dropped = st
            .with_user_transaction(&mut txn, || st.drop_index("app", "c", "x_1"))
            .unwrap()
            .unwrap();
        assert!(dropped);
        st.rollback_user_transaction(&mut txn).unwrap();
        // The registry row AND the entry rows came back together — a reader
        // routing through the index finds its entries intact.
        assert!(index_names(st, "app", "c").contains(&"x_1".to_string()));
        let found = st.find_matching("app", "c", &doc! {"x": 5}).unwrap();
        assert_eq!(found.len(), 1);
    });
}

#[test]
fn drop_collection_rolls_back_inside_user_transaction() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 5}))
            .unwrap();
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &Document::new())
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let existed = st
            .with_user_transaction(&mut txn, || st.drop_collection("app", "c"))
            .unwrap()
            .unwrap();
        assert!(existed);
        st.rollback_user_transaction(&mut txn).unwrap();
        // Docs, index registry and entries all survive the rollback.
        let found = st.find_matching("app", "c", &doc! {"x": 5}).unwrap();
        assert_eq!(found.len(), 1);
        assert!(index_names(st, "app", "c").contains(&"x_1".to_string()));
    });
}

#[test]
fn rename_collection_rolls_back_inside_user_transaction() {
    with_db(|st| {
        st.insert_one("app", "src", &enc(&doc! {"_id": 1, "x": 5}))
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let (ok, err) = st
            .with_user_transaction(&mut txn, || {
                st.rename_collection("app", "src", "app", "dst", false)
            })
            .unwrap()
            .unwrap();
        assert!(ok, "{err:?}");
        st.rollback_user_transaction(&mut txn).unwrap();
        // The move reverted whole: src intact, dst gone.
        let src = st.find_matching("app", "src", &doc! {}).unwrap();
        assert_eq!(src.len(), 1);
        let dst = st.find_matching("app", "dst", &doc! {}).unwrap();
        assert!(dst.is_empty());
    });
}

#[test]
fn create_collection_commit_persists_options_and_registry_together() {
    with_db(|st| {
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_collection_with_options(
                "app",
                "capped",
                &doc! {"capped": true, "size": 4096i64},
            )
        })
        .unwrap()
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        let opts = st.get_collection_options("app", "capped").unwrap();
        assert_eq!(opts.get("capped"), Some(&Bson::Boolean(true)));
    });
}
