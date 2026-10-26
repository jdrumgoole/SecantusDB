//! Reads at a past timestamp (RM-210): a store whose data tables are not
//! logged stamps every write with a WiredTiger commit timestamp, and
//! `read_at` reads as of one. Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::{Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-snap-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(home: &std::path::Path, nonlogged: bool) -> Storage {
    Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            data_nonlogged: Some(nonlogged),
            transactions_logged_at_commit: true,
            ..StorageOptions::default()
        },
    )
    .unwrap()
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// `(_id, n)` of every document, by `_id`.
fn rows(st: &Storage) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = st
        .find_matching("app", "c", &Document::new())
        .unwrap()
        .iter()
        .map(|b| bson::from_slice::<Document>(b).unwrap())
        .map(|d| (d.get_i32("_id").unwrap(), d.get_i32("n").unwrap_or(-1)))
        .collect();
    out.sort();
    out
}

fn set_n(st: &Storage, id: i32, n: i32) {
    st.update_matching(
        "app",
        "c",
        &doc! {"_id": id},
        &doc! {"$set": {"n": n}},
        false,
        false,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
    .unwrap();
}

#[test]
fn a_read_at_a_past_timestamp_sees_the_data_as_it_was() {
    let home = temp_home();
    let st = open(&home, true);
    assert!(st.is_timestamped());
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
        .unwrap();
    let t0 = st.all_committed_ts();
    set_n(&st, 1, 9);
    st.insert_one("app", "c", &enc(&doc! {"_id": 2, "n": 5}))
        .unwrap();
    let t1 = st.all_committed_ts();
    assert_eq!(st.read_at(t0, || rows(&st)).unwrap(), vec![(1, 0)]);
    assert_eq!(st.read_at(t1, || rows(&st)).unwrap(), vec![(1, 9), (2, 5)]);
    assert_eq!(rows(&st), vec![(1, 9), (2, 5)]);
}

#[test]
fn a_transaction_is_seen_whole_or_not_at_all() {
    let home = temp_home();
    let st = open(&home, true);
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
        .unwrap();
    let before = st.all_committed_ts();
    let mut txn = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "n": 2}))
    })
    .unwrap()
    .unwrap();
    // A write outside it, minted between the transaction's two statements.
    st.insert_one("app", "c", &enc(&doc! {"_id": 4, "n": 4}))
        .unwrap();
    let between = st.peek_cluster_time().unwrap();
    // The transaction takes its time when it commits, as mongod's does: a
    // read at the time of the write made between its statements does not
    // wait for it, and never sees it.
    assert!(st.all_committed_ts() >= between);
    assert_eq!(
        st.read_at(between, || rows(&st)).unwrap(),
        vec![(1, 0), (4, 4)]
    );
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "n": 3}))
    })
    .unwrap()
    .unwrap();
    st.commit_user_transaction(&mut txn).unwrap();
    let after = st.all_committed_ts();
    assert_eq!(st.read_at(before, || rows(&st)).unwrap(), vec![(1, 0)]);
    assert_eq!(
        st.read_at(between, || rows(&st)).unwrap(),
        vec![(1, 0), (4, 4)]
    );
    assert_eq!(
        st.read_at(after, || rows(&st)).unwrap(),
        vec![(1, 0), (2, 2), (3, 3), (4, 4)]
    );
}

#[test]
fn history_survives_a_close_and_reopen() {
    let home = temp_home();
    let t0;
    {
        let st = open(&home, true);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
            .unwrap();
        t0 = st.all_committed_ts();
        set_n(&st, 1, 7);
        st.close().unwrap();
    }
    let st = open(&home, true);
    assert_eq!(rows(&st), vec![(1, 7)]);
    assert_eq!(st.read_at(t0, || rows(&st)).unwrap(), vec![(1, 0)]);
    // Writes after the reopen are later than everything before it.
    set_n(&st, 1, 8);
    assert_eq!(st.read_at(t0, || rows(&st)).unwrap(), vec![(1, 0)]);
    assert_eq!(rows(&st), vec![(1, 8)]);
}

#[test]
fn a_collection_created_later_is_not_there_at_an_earlier_time() {
    let home = temp_home();
    let st = open(&home, true);
    st.insert_one("app", "other", &enc(&doc! {"_id": 1}))
        .unwrap();
    let before = st.all_committed_ts();
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 1}))
        .unwrap();
    assert_eq!(st.read_at(before, || rows(&st)).unwrap(), vec![]);
    assert_eq!(rows(&st), vec![(1, 1)]);
}

#[test]
fn a_logged_store_keeps_no_history() {
    let home = temp_home();
    let st = open(&home, false);
    assert!(!st.is_timestamped());
    assert!(st.read_at(st.all_committed_ts(), || ()).is_err());
}
