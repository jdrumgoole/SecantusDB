//! RM-746: the stable timestamp, which a checkpoint keeps as its recovery
//! timestamp, is always an oplog entry's, as mongod's is (its stable optime
//! candidates are the entries it wrote). A timestamp consumed without an
//! entry (a cluster-time mint) or a write that logs none (unreplicated, or
//! of `local`) never moves it past the oplog's end, and an operation's write
//! without an entry of its own commits at the timestamp of the operation's
//! entry ([`Storage::op_scope`]), which mongod's single unit of work gives it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use bson::{doc, Document};
use secantus_storage::{Storage, StorageOptions};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-stable-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(home: &std::path::Path) -> Storage {
    Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            data_nonlogged: Some(true),
            oplog_nonlogged: Some(false),
            ..StorageOptions::default()
        },
    )
    .unwrap()
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// The newest oplog entry's position (its timestamp).
fn last_entry(st: &Storage) -> i64 {
    let mut from = 0;
    let mut last = 0;
    loop {
        let rows = st.read_oplog(from, 1000).unwrap();
        let Some((seq, _)) = rows.last() else {
            return last;
        };
        last = *seq;
        from = seq + 1;
    }
}

/// Every oplog entry's position.
fn entries(st: &Storage) -> Vec<i64> {
    st.read_oplog(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(seq, _)| seq)
        .collect()
}

#[test]
fn the_stable_timestamp_is_the_newest_entry_not_a_later_mint() {
    let home = temp_home();
    let st = open(&home);
    st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
    // Timestamps consumed without an entry: cluster-time mints, and a write
    // that logs none.
    for _ in 0..5 {
        st.current_cluster_time().unwrap();
    }
    {
        let _quiet = secantus_storage::unreplicated();
        st.insert_one("app", "quiet", &enc(&doc! {"_id": 1}))
            .unwrap();
    }
    st.insert_one("local", "scratch", &enc(&doc! {"_id": 1}))
        .unwrap();
    for _ in 0..5 {
        st.current_cluster_time().unwrap();
    }
    let minted = st.all_committed_ts();
    st.stable_checkpoint().unwrap();
    let stable = st.stable_checkpoint_seq();
    let last = last_entry(&st);
    assert_eq!(stable, last, "the stable timestamp is the newest entry's");
    assert!(entries(&st).contains(&stable));
    let minted = ((minted.time as i64) << 32) | minted.increment as i64;
    assert!(minted > last, "the mints went past the last entry");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_write_without_an_entry_in_an_operation_commits_at_its_entry() {
    let home = temp_home();
    let st = open(&home);
    st.insert_one("app", "c", &enc(&doc! {"_id": 0})).unwrap();
    let before = st.all_committed_ts();
    let entry_ts;
    {
        let _op = st.op_scope();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        entry_ts = last_entry(&st);
        // The operation holds the stable timestamp below its entry until it
        // ends, so the write that follows can commit at the entry's.
        let held = st.all_committed_ts();
        let held = ((held.time as i64) << 32) | held.increment as i64;
        assert!(
            held < entry_ts,
            "the operation holds the all-committed point"
        );
        let _quiet = secantus_storage::unreplicated();
        st.insert_one(
            "config",
            "transactions",
            &enc(&doc! {"_id": 1, "txnNum": 1}),
        )
        .unwrap();
    }
    let after = st.all_committed_ts();
    assert!(after > before);
    // At the entry's timestamp the row is there; just before it, neither is.
    let at = bson::Timestamp {
        time: (entry_ts >> 32) as u32,
        increment: entry_ts as u32,
    };
    let below = bson::Timestamp {
        time: ((entry_ts - 1) >> 32) as u32,
        increment: (entry_ts - 1) as u32,
    };
    let count = |t: bson::Timestamp| {
        st.read_at(t, || {
            (
                st.find_by_id("app", "c", &bson::Bson::Int32(1))
                    .unwrap()
                    .is_some(),
                st.count_matching("config", "transactions", &Document::new(), None)
                    .unwrap(),
            )
        })
        .unwrap()
    };
    assert_eq!(count(at), (true, 1));
    assert_eq!(count(below), (false, 0));
    // No mint, no hold, is left behind: the stable timestamp reaches the
    // entry once the operation has ended.
    st.stable_checkpoint().unwrap();
    assert_eq!(st.stable_checkpoint_seq(), last_entry(&st));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
