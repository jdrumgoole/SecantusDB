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

/// A user transaction's writes are held to its commit, as mongod holds a
/// transaction's operations: an open transaction has minted nothing, so it
/// pins nothing and a write committed beside it is visible at once; at its
/// commit the transaction is one entry, after those. (A position is the
/// entry's timestamp: `tail` is the position of the newest entry minted.)
#[test]
fn an_open_transaction_logs_nothing_until_it_commits() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let p1 = st.oplog_tail_seq();
        assert_eq!(st.wait_for_oplog(0, 10), p1);

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "x", &enc(&doc! {"_id": 1}))
        })
        .unwrap()
        .unwrap();
        // Nothing minted, so nothing in flight.
        assert_eq!(st.oplog_tail_seq(), p1);

        // A different write commits beside it and is visible at once.
        st.insert_one("app", "y", &enc(&doc! {"_id": 2})).unwrap();
        let p3 = st.oplog_tail_seq();
        assert!(p3 > p1);
        assert_eq!(st.wait_for_oplog(p1, 10), p3);
        let seqs: Vec<i64> = st
            .read_oplog(p1 + 1, 100)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(seqs, vec![p3], "the insert only");
        let before = seqs.len();

        // The commit logs the transaction, after those, as one entry.
        st.commit_user_transaction(&mut txn).unwrap();
        let rows = st.read_oplog(p1 + 1, 100).unwrap();
        assert_eq!(rows.len(), before + 1);
        let (seq, blob) = rows.last().unwrap();
        assert!(*seq > p3);
        assert_eq!(st.wait_for_oplog(p1, 10), *seq);
        let entry: Document = bson::from_slice(blob).unwrap();
        assert_eq!(entry.get_str("op"), Ok("c"));
        assert_eq!(
            entry
                .get_document("o")
                .unwrap()
                .get_array("applyOps")
                .unwrap()
                .len(),
            1
        );
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

/// `find_seq_for_ts` (the startAtOperationTime mapping) answers from what is
/// committed at once while a user transaction is open: its writes are held to
/// its commit and mint nothing, so there is no in-flight window to wait out.
#[test]
fn find_seq_for_ts_does_not_wait_on_an_open_transaction() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let rows = st.read_oplog(st.oplog_tail_seq(), 1).unwrap();
        let ts1 = ts_of(&rows[0].1);
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

        let started = std::time::Instant::now();
        assert_eq!(st.find_seq_for_ts(target).unwrap(), p3);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "it waited on a transaction that minted nothing"
        );
        st.commit_user_transaction(&mut txn).unwrap();
    });
}

/// The rollback arm: an abandoned in-flight mint resolves the wait and the
/// committed-view answer stands (the hole is permanent).
#[test]
fn find_seq_for_ts_rolled_back_mint_returns_committed_answer() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let ts1 = ts_of(&st.read_oplog(st.oplog_tail_seq(), 1).unwrap()[0].1);
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

/// A multi-statement transaction is one entry, at its commit, holding every
/// statement's writes; nothing of it shows before.
#[test]
fn a_multi_statement_txn_is_one_entry_at_its_commit() {
    with_db(|st| {
        st.insert_one("app", "x", &enc(&doc! {"_id": 0})).unwrap();
        let p1 = st.oplog_tail_seq();

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
        assert_eq!(st.oplog_tail_seq(), p1);
        assert!(st.read_oplog(p1 + 1, 100).unwrap().is_empty());

        st.commit_user_transaction(&mut txn).unwrap();
        let rows = st.read_oplog(p1 + 1, 100).unwrap();
        assert_eq!(rows.len(), 1, "one entry");
        let entry: Document = bson::from_slice(&rows[0].1).unwrap();
        let ops: Vec<&str> = entry
            .get_document("o")
            .unwrap()
            .get_array("applyOps")
            .unwrap()
            .iter()
            .map(|o| o.as_document().unwrap().get_str("ns").unwrap())
            .collect();
        assert_eq!(ops, vec!["app.x", "app.y"]);
        assert!(entry.contains_key("lsid") && entry.contains_key("txnNumber"));
    });
}
