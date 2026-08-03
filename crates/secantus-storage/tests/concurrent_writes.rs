//! Per-collection write locks + write-conflict machinery: writers to
//! different collections run in parallel, writers to the same collection
//! serialise with exact-count outcomes, unique races have exactly one
//! winner, DDL excludes in-flight CRUD on its namespace, and a plain write
//! racing a user transaction retries to completion instead of surfacing a
//! WriteConflict. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-concwrite-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

fn inc_by_one(st: &Storage, db: &str, coll: &str, id: i32) {
    st.update_matching(
        db,
        coll,
        &doc! {"_id": id},
        &doc! {"$inc": {"n": 1}},
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
fn cross_collection_writers_land_every_write() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    let threads = 6;
    let per = 100;
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let st = Arc::clone(&st);
            thread::spawn(move || {
                let coll = format!("c{t}");
                for i in 0..per {
                    st.insert_one("app", &coll, &enc(&doc! {"_id": i, "t": t}))
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    for t in 0..threads {
        let coll = format!("c{t}");
        assert_eq!(
            st.count_matching("app", &coll, &Document::new(), None)
                .unwrap(),
            per as usize,
            "collection {coll} lost writes"
        );
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn same_collection_inc_hammer_is_exact() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
        .unwrap();
    let threads = 8;
    let per = 50;
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let st = Arc::clone(&st);
            thread::spawn(move || {
                for _ in 0..per {
                    inc_by_one(&st, "app", "c", 1);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let blob = st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap();
    assert_eq!(
        decode(&blob).get_i32("n").unwrap(),
        threads * per,
        "lost increments under the per-collection lock"
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unique_index_race_has_exactly_one_winner() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    st.create_collection("app", "c").unwrap();
    st.create_index("app", "c", "u_1", &doc! {"u": 1}, &doc! {"unique": true})
        .unwrap();
    let threads = 8;
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let st = Arc::clone(&st);
            thread::spawn(move || {
                match st.insert_one("app", "c", &enc(&doc! {"_id": t, "u": "same"})) {
                    Ok(_) => Ok(()),
                    Err(StorageError::DuplicateKey(_)) => Err(()),
                    Err(e) => panic!("loser saw an untyped error: {e}"),
                }
            })
        })
        .collect();
    let wins = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(Result::is_ok)
        .count();
    assert_eq!(wins, 1, "unique race must have exactly one winner");
    assert_eq!(
        st.count_matching("app", "c", &doc! {"u": "same"}, None)
            .unwrap(),
        1
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn create_index_under_write_load_is_complete() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    for i in 0..200 {
        st.insert_one("app", "c", &enc(&doc! {"_id": i, "g": i % 5}))
            .unwrap();
    }
    let writer = {
        let st = Arc::clone(&st);
        thread::spawn(move || {
            for i in 200..500 {
                st.insert_one("app", "c", &enc(&doc! {"_id": i, "g": i % 5}))
                    .unwrap();
            }
        })
    };
    // Build the index while the writer is mid-storm. The DDL lock discipline
    // (global + collection) makes the backfill atomic w.r.t. the writer, so
    // every doc — backfilled or freshly inserted — must be reachable through
    // the index.
    st.create_index("app", "c", "g_1", &doc! {"g": 1}, &Document::new())
        .unwrap();
    writer.join().unwrap();
    let total: usize = (0..5)
        .map(|g| st.find_matching("app", "c", &doc! {"g": g}).unwrap().len())
        .sum();
    assert_eq!(total, 500, "index-routed reads missed documents");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn plain_write_racing_user_txn_retries_to_completion() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
        .unwrap();

    // The transaction takes the first update and parks uncommitted.
    let mut handle = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut handle, || inc_by_one(&st, "app", "c", 1))
        .unwrap();

    // A plain writer on another thread hits the uncommitted write and must
    // retry (never surface WriteConflict to its caller).
    let racer = {
        let st = Arc::clone(&st);
        thread::spawn(move || inc_by_one(&st, "app", "c", 1))
    };
    thread::sleep(Duration::from_millis(150));
    st.commit_user_transaction(&mut handle).unwrap();
    racer.join().expect("plain writer surfaced an error");

    let blob = st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap();
    assert_eq!(
        decode(&blob).get_i32("n").unwrap(),
        2,
        "both the transactional and the retried plain increment must land"
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn user_txn_statement_conflict_surfaces_write_conflict() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
        .unwrap();

    let mut a = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut a, || inc_by_one(&st, "app", "c", 1))
        .unwrap();

    // A second transaction touching the same document must get the typed
    // WriteConflict at statement time (no retry inside a user transaction).
    let mut b = st.begin_user_transaction().unwrap();
    let res = st
        .with_user_transaction(&mut b, || {
            st.update_matching(
                "app",
                "c",
                &doc! {"_id": 1},
                &doc! {"$inc": {"n": 1}},
                false,
                false,
                &[],
                &Document::new(),
                None,
                None,
                false,
            )
        })
        .unwrap();
    match res {
        Err(StorageError::WriteConflict) => {}
        other => panic!("expected WriteConflict inside the transaction, got {other:?}"),
    }
    st.rollback_user_transaction(&mut b).unwrap();
    st.commit_user_transaction(&mut a).unwrap();

    let blob = st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap();
    assert_eq!(decode(&blob).get_i32("n").unwrap(), 1);
    // Close the WiredTiger connection (final checkpoint + background-thread join)
    // BEFORE removing its data dir. Without this, WT's log/eviction/close-checkpoint
    // threads operate on a deleted directory and WT_PANIC ("WiredTigerHS.wt: No such
    // file"); the assertions above still pass, so it only shows as teardown log
    // noise. The other tests in this file already do `drop(st)` first.
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
