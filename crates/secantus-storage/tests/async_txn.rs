//! Async-oplog user-transaction correctness: a transaction's statements
//! buffer their oplog entries on the handle, and they reach the drainer only
//! after the WT commit succeeds. A rolled-back transaction must leave NO
//! oplog trace — before the fix, `emit_oplog_entries` saw `IN_ASYNC_STMT`
//! false inside a user transaction (the `with_statement_txn` early-return
//! skips the flag) and minted + enqueued mid-transaction, so a rollback left
//! a persisted ghost entry: a change event / PITR row for data that never
//! committed. Against real WiredTiger; async mode pinned via `StorageOptions`
//! (the option beats the env var, so this holds in every CI lane).

use bson::{doc, Document};
use secantus_storage::{Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-asynctxn-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn with_async_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            oplog_async: Some(true),
            ..StorageOptions::default()
        },
    )
    .unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    bson::from_slice(b).unwrap()
}

#[test]
fn rolled_back_txn_leaves_no_oplog_trace() {
    with_async_db(|st| {
        // Baseline committed write (seq 1) so the assertion below can tell
        // "rollback left nothing" apart from "nothing works at all".
        st.insert_one("app", "c", &enc(&doc! {"_id": 0})).unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "c", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "rolled-back transaction must not surface an oplog entry"
        );
        // The data really rolled back too — entry-less is consistent, not a
        // fluke of the read path.
        assert_eq!(st.scan_collection("app", "c").unwrap().len(), 1);
    });
}

#[test]
fn committed_txn_entries_land_exactly_once_after_commit() {
    with_async_db(|st| {
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "c", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "c", &enc(&doc! {"_id": 2}))
        })
        .unwrap()
        .unwrap();
        // Mid-transaction: nothing minted, nothing at the drainer — the
        // oplog must be empty even after a flush.
        st.flush_oplog();
        assert!(
            st.read_oplog(1, 100).unwrap().is_empty(),
            "entries must not reach the oplog before the commit"
        );
        st.commit_user_transaction(&mut txn).unwrap();
        st.flush_oplog();
        let ops: Vec<String> = st
            .read_oplog(1, 100)
            .unwrap()
            .iter()
            .map(|(_, b)| decode(b).get_str("op").unwrap().to_string())
            .collect();
        assert_eq!(ops, vec!["i", "i"], "exactly the two inserts, exactly once");
    });
}
