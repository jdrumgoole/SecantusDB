//! Oplog visibility-point tests: the sync-mode tail must never pass a
//! minted-but-uncommitted entry (the minted-vs-committed race), and a rolled
//! back mint must not stall the tail. Against real WiredTiger.
//!
//! The bug these pin (pre-fix): `wait_for_oplog`'s sync tail was
//! `next_seq - 1` — the highest *minted* seq. A writer holding an open
//! transaction that already emitted (minted) an oplog entry left a hole below
//! the reported tail; a change-stream poll that scanned past the hole and
//! advanced its resume position lost the entry permanently once it committed.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-oplogvis-{}-{}", std::process::id(), n));
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

/// A minted-but-uncommitted entry (open user transaction) must pin the
/// visible tail below it: `wait_for_oplog` must not report past the hole and
/// `read_oplog` must not serve later-committed rows across it. Once the
/// transaction commits, everything becomes visible in seq order.
#[test]
fn in_flight_txn_pins_visible_tail() {
    with_db(|st| {
        // seq 1: plain committed insert (baseline position).
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        assert_eq!(st.wait_for_oplog(0, 10), 1);

        // seq 2: minted inside an OPEN user transaction — uncommitted.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();

        // seq 3: a different collection commits AFTER the in-flight mint.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();

        // The visible tail must stay pinned at 1: seq 2 is in flight, so
        // reporting 3 would let a reader advance past the hole and lose 2.
        assert_eq!(
            st.wait_for_oplog(1, 10),
            1,
            "tail passed a minted-but-uncommitted entry"
        );
        // And the scan must not serve seq 3 across the in-flight hole.
        let rows = st.read_oplog(2, 100).unwrap();
        assert!(
            rows.is_empty(),
            "read_oplog served rows past an in-flight mint: {:?}",
            rows.iter().map(|(s, _)| *s).collect::<Vec<_>>()
        );

        // Commit resolves the hole: everything visible, in order.
        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(st.wait_for_oplog(1, 10), 3);
        let seqs: Vec<i64> = st
            .read_oplog(2, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![2, 3]);
    });
}

/// A rolled-back mint is a permanent hole: the tail must advance past it
/// (no stalled watermark) and the scan must serve the later rows.
#[test]
fn rolled_back_mint_does_not_stall_tail() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();

        // seq 2 minted in a txn that rolls back — never becomes visible.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();

        // seq 3: committed after the abandoned mint.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();

        // The abandoned range must not pin the tail forever.
        assert_eq!(
            st.wait_for_oplog(1, 10),
            3,
            "tail stalled on a rolled-back mint"
        );
        let seqs: Vec<i64> = st
            .read_oplog(2, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![3]);
    });
}

/// Multi-statement transaction: every statement's mint joins the same
/// in-flight window; nothing leaks visible until the single commit.
#[test]
fn multi_statement_txn_holds_all_mints_until_commit() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "y", &enc(&doc! {"_id": 2}))
        })
        .unwrap()
        .unwrap();

        // Concurrent autocommit writer lands seq 4.
        st.insert_one("app", "z", &enc(&doc! {"_id": 3})).unwrap();

        assert_eq!(st.wait_for_oplog(1, 10), 1);
        assert!(st.read_oplog(2, 100).unwrap().is_empty());

        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(st.wait_for_oplog(1, 10), 4);
        let seqs: Vec<i64> = st
            .read_oplog(2, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![2, 3, 4]);
    });
}
