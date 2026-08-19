//! The multi-document-transaction dirty budget (mongod's
//! TransactionTooLargeForCache): a transaction's writes are unevictable
//! until commit, so an unbounded one livelocks WiredTiger — the guard trips
//! first, sized off the cache. Against real WiredTiger with a deliberately
//! small cache.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-txnbudget-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

#[test]
fn transaction_dirty_budget_guard() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("128M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let filler = "x".repeat(1024 * 1024);
    // 128M cache -> budget = 128M * 0.15 = ~19.2M; trips when 2x emitted
    // bytes exceed it, i.e. past ~10 one-MB documents.
    let mut txn = st.begin_user_transaction().unwrap();
    let mut tripped = false;
    for i in 0..32i64 {
        let doc_bytes = enc(&doc! {"_id": i, "pad": filler.clone()});
        // The budget can trip at EITHER level, and both are correct:
        //
        //   * the OUTER Err is our own post-statement guard, sized from the
        //     cache;
        //   * the INNER Err is WiredTiger hitting its dirty limit DURING the
        //     statement and rolling back, which the rollback-reason check maps
        //     to the same error.
        //
        // Accepting only the outer one made this test fail on a loaded runner
        // whenever the engine got there first — the flake the rollback-reason
        // classification was written for. It cannot be reproduced on a fast
        // machine (80 runs under 8-way concurrency did not), so CI is where
        // this branch actually gets exercised.
        match st.with_user_transaction(&mut txn, || st.insert("app", "c", vec![doc_bytes], true)) {
            Ok(Ok(_)) => {}
            Ok(Err(StorageError::TransactionTooLargeForCache))
            | Err(StorageError::TransactionTooLargeForCache) => {
                tripped = true;
                break;
            }
            Ok(Err(inner)) => panic!("unexpected error from the statement: {inner}"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(
        tripped,
        "an oversized transaction must trip the dirty budget"
    );
    st.rollback_user_transaction(&mut txn).unwrap();
    // Nothing from the aborted transaction is visible.
    assert!(st
        .find_by_id("app", "c", &Bson::Int64(0))
        .unwrap()
        .is_none());
    // The same volume outside a transaction is fine (chunked statements).
    let docs: Vec<Vec<u8>> = (0..32i64)
        .map(|i| enc(&doc! {"_id": i, "pad": filler.clone()}))
        .collect();
    let (inserted, errors) = st.insert("app", "c", docs, true).unwrap();
    assert_eq!(inserted, 32);
    assert!(errors.is_empty());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn async_transaction_dirty_budget_bounds_pending_async() {
    // #750: in async-oplog mode a transaction's statements buffer their oplog
    // entries in `UserTransactionHandle::pending_async` for the whole
    // transaction lifetime. Those bytes are charged to the same dirty budget as
    // the sync path (the accounting runs BEFORE the async buffering branch), so
    // an oversized async transaction trips `TransactionTooLargeForCache` rather
    // than growing the buffer without bound.
    let home = temp_home();
    let st = Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            wt_config: Some(secantus_storage::wt_config("128M", 1000, false, "10MB")),
            oplog_async: Some(true),
            ..StorageOptions::default()
        },
    )
    .unwrap();
    let filler = "x".repeat(1024 * 1024);
    let mut txn = st.begin_user_transaction().unwrap();
    let mut tripped = false;
    for i in 0..32i64 {
        let doc_bytes = enc(&doc! {"_id": i, "pad": filler.clone()});
        match st.with_user_transaction(&mut txn, || st.insert("app", "c", vec![doc_bytes], true)) {
            Ok(inner) => {
                inner.unwrap();
            }
            Err(StorageError::TransactionTooLargeForCache) => {
                tripped = true;
                break;
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(
        tripped,
        "an oversized async transaction must trip the dirty budget, not buffer unbounded"
    );
    st.rollback_user_transaction(&mut txn).unwrap();
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
