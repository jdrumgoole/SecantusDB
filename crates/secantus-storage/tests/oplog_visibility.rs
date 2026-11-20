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
use secantus_storage::{Storage, StorageOptions};
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
    let mut st =
        Storage::open_with_options(home.to_str().unwrap(), &StorageOptions::default()).unwrap();
    // The collections written below exist before the first counted write:
    // a write that creates its collection records the creation too.
    st.set_enable_oplog(false);
    for coll in ["x", "y", "z"] {
        st.create_collection("app", coll).unwrap();
    }
    st.set_enable_oplog(true);
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
/// transaction commits, everything becomes visible in position order. (A
/// position is the entry's timestamp: `tail` below is the position of the
/// newest entry minted, committed or not.)
#[test]
fn in_flight_txn_pins_visible_tail() {
    with_db(|st| {
        // p1: plain committed insert (baseline position).
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let p1 = st.oplog_tail_seq();
        assert_eq!(st.wait_for_oplog(0, 10), p1);

        // p2: minted inside an OPEN user transaction -- uncommitted.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        let p2 = st.oplog_tail_seq();
        assert!(p2 > p1);

        // p3: a different collection commits AFTER the in-flight mint.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();
        let p3 = st.oplog_tail_seq();

        // The visible tail must stay below p2: it is in flight, so
        // reporting p3 would let a reader advance past the hole and lose it.
        let tail = st.wait_for_oplog(p1, 10);
        assert!(
            (p1..p2).contains(&tail),
            "tail {tail} passed a minted-but-uncommitted entry at {p2}"
        );
        // And the scan must not serve p3 across the in-flight hole.
        let rows = st.read_oplog(p1 + 1, 100).unwrap();
        assert!(
            rows.is_empty(),
            "read_oplog served rows past an in-flight mint: {:?}",
            rows.iter().map(|(s, _)| *s).collect::<Vec<_>>()
        );

        // Commit resolves the hole: everything visible, in order.
        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(st.wait_for_oplog(p1, 10), p3);
        let seqs: Vec<i64> = st
            .read_oplog(p1 + 1, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![p2, p3]);
    });
}

/// A rolled-back mint is a permanent hole: the tail must advance past it
/// (no stalled watermark) and the scan must serve the later rows.
#[test]
fn rolled_back_mint_does_not_stall_tail() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let p1 = st.oplog_tail_seq();

        // p2 minted in a txn that rolls back -- never becomes visible.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();

        // p3: committed after the abandoned mint.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();
        let p3 = st.oplog_tail_seq();

        // The abandoned range must not pin the tail forever.
        assert_eq!(
            st.wait_for_oplog(p1, 10),
            p3,
            "tail stalled on a rolled-back mint"
        );
        let seqs: Vec<i64> = st
            .read_oplog(p1 + 1, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![p3]);
    });
}

fn ts_of(blob: &[u8]) -> bson::Timestamp {
    let d = Document::from_reader(&mut std::io::Cursor::new(blob)).unwrap();
    match d.get("ts") {
        Some(bson::Bson::Timestamp(t)) => *t,
        other => panic!("expected ts Timestamp, got {other:?}"),
    }
}

/// `find_seq_for_ts` (the startAtOperationTime mapping) must not finalise a
/// position past an in-flight mint whose entry also qualifies: it waits for
/// the in-flight window to drain past its committed-view answer and rescans,
/// so a commit that lands mid-wait surfaces the earlier seq.
#[test]
fn find_seq_for_ts_waits_for_in_flight_mint() {
    with_db(|st| {
        // seq 1 (committed): gives us a readable ts to anchor the target.
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let rows = st.read_oplog(1, 1).unwrap();
        let ts1 = ts_of(&rows[0].1);
        let _p1 = rows[0].0;
        // Target: strictly after seq 1's ts, at or before every later mint.
        let target = bson::Timestamp {
            time: ts1.time,
            increment: ts1.increment + 1,
        };

        // seq 2: minted in an OPEN transaction — its ts satisfies the target,
        // but the committed view's first match is seq 3.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        let p2 = st.oplog_tail_seq();
        // seq 3: committed after the in-flight mint.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();

        // Commit the transaction from another thread while find_seq_for_ts
        // waits on the in-flight window.
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(120));
                st.commit_user_transaction(&mut txn).unwrap();
            });
            let seq = st.find_seq_for_ts(target).unwrap();
            assert_eq!(
                seq, p2,
                "startAtOperationTime finalised past an in-flight mint"
            );
        });
    });
}

/// The rollback arm: an abandoned in-flight mint resolves the wait and the
/// committed-view answer stands (the hole is permanent).
#[test]
fn find_seq_for_ts_rolled_back_mint_returns_committed_answer() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let ts1 = ts_of(&st.read_oplog(1, 1).unwrap()[0].1);
        let target = bson::Timestamp {
            time: ts1.time,
            increment: ts1.increment + 1,
        };

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();
        let p3 = st.oplog_tail_seq();

        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(120));
                st.rollback_user_transaction(&mut txn).unwrap();
            });
            let seq = st.find_seq_for_ts(target).unwrap();
            assert_eq!(
                seq, p3,
                "rolled-back mint should leave the committed answer"
            );
        });
    });
}

/// Multi-statement transaction: every statement's mint joins the same
/// in-flight window; nothing leaks visible until the single commit.
#[test]
fn multi_statement_txn_holds_all_mints_until_commit() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let p1 = st.oplog_tail_seq();

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        let p2 = st.oplog_tail_seq();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "y", &enc(&doc! {"_id": 2}))
        })
        .unwrap()
        .unwrap();
        let p3 = st.oplog_tail_seq();

        // Concurrent autocommit writer lands the fourth.
        st.insert_one("app", "z", &enc(&doc! {"_id": 3})).unwrap();
        let p4 = st.oplog_tail_seq();

        assert!((p1..p2).contains(&st.wait_for_oplog(p1, 10)));
        assert!(st.read_oplog(p1 + 1, 100).unwrap().is_empty());

        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(st.wait_for_oplog(p1, 10), p4);
        let seqs: Vec<i64> = st
            .read_oplog(p1 + 1, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![p2, p3, p4]);
    });
}
