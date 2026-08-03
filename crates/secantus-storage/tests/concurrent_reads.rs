//! Read-only methods run lock-free against live writers: readers must never
//! error, never observe a document that fails the filter they asked for, and
//! never block behind the write lock. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-concread-{}-{}", std::process::id(), n));
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

/// Four readers hammer `find_by_id` / `find_matching` / `scan_collection` /
/// `count_matching` / `list_indexes` while a writer replaces, deletes and
/// re-inserts documents and churns a secondary index. Every result a reader
/// gets back must be internally consistent: decodable, and matching the
/// filter it asked for. (Result *sets* may reflect any point in the churn —
/// that is mongod's own yield-and-refresh collscan semantics — but a
/// non-matching or torn document is a bug.)
#[test]
fn readers_stay_consistent_under_write_churn() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    let n_docs = 200i32;
    for i in 0..n_docs {
        st.insert_one("app", "c", &enc(&doc! {"_id": i, "grp": i % 10, "val": i}))
            .unwrap();
    }
    st.create_index("app", "c", "grp_1", &doc! {"grp": 1}, &Document::new())
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let st = Arc::clone(&st);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut round = 0i32;
            while round < 60 {
                for i in (0..n_docs).step_by(7) {
                    // Replace keeps _id and grp stable, moves val.
                    st.replace_by_id(
                        "app",
                        "c",
                        &Bson::Int32(i),
                        &enc(&doc! {"grp": i % 10, "val": round * 1000 + i}),
                    )
                    .unwrap();
                }
                for i in (3..n_docs).step_by(29) {
                    st.delete_by_id("app", "c", &Bson::Int32(i)).unwrap();
                    st.insert_one(
                        "app",
                        "c",
                        &enc(&doc! {"_id": i, "grp": i % 10, "val": -round}),
                    )
                    .unwrap();
                }
                // Index churn: drop + recreate the secondary index mid-read.
                if round % 15 == 14 {
                    st.drop_index("app", "c", "grp_1").unwrap();
                    st.create_index("app", "c", "grp_1", &doc! {"grp": 1}, &Document::new())
                        .unwrap();
                }
                round += 1;
            }
            stop.store(true, Ordering::SeqCst);
        })
    };

    let mut readers = Vec::new();
    for r in 0..4 {
        let st = Arc::clone(&st);
        let stop = Arc::clone(&stop);
        readers.push(thread::spawn(move || {
            let mut iters = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let grp = (r * 3) % 10;
                let hits = st
                    .find_matching("app", "c", &doc! {"grp": grp})
                    .expect("find_matching errored under churn");
                for blob in &hits {
                    let d = decode(blob);
                    assert_eq!(
                        d.get_i32("grp").unwrap(),
                        grp,
                        "reader served a document that does not match its filter"
                    );
                }
                let one = st
                    .find_by_id("app", "c", &Bson::Int32(grp * 11))
                    .expect("find_by_id errored under churn");
                if let Some(blob) = one {
                    assert_eq!(decode(&blob).get_i32("_id").unwrap(), grp * 11);
                }
                let all = st
                    .scan_collection("app", "c")
                    .expect("scan_collection errored under churn");
                for blob in &all {
                    decode(blob); // must stay decodable — no torn values
                }
                st.count_matching("app", "c", &doc! {"grp": grp}, None)
                    .expect("count_matching errored under churn");
                st.list_indexes("app", "c")
                    .expect("list_indexes errored under churn");
                iters += 1;
            }
            iters
        }));
    }

    writer.join().expect("writer panicked");
    for h in readers {
        let iters = h.join().expect("reader panicked");
        assert!(iters > 0, "reader never completed an iteration");
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A lock-free scan racing a namespace-level DDL (drop / rename) must return a
/// point-in-time answer: either the whole pre-DDL result set or the post-DDL
/// one — never a partial splice of the two. The DDL's row writes commit in one
/// statement transaction and bump the storage's DDL generation; readers re-run
/// a scan whose generation moved (the DDL-vs-scan wobble fix). The drop and
/// rename rounds each re-seed, so every observed result must be exactly the
/// full set or empty.
#[test]
fn scans_racing_namespace_ddl_are_never_partial() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    let n_docs = 300usize;
    let seed = |coll: &str| {
        for i in 0..n_docs as i32 {
            st.insert_one("app", coll, &enc(&doc! {"_id": i, "x": 1}))
                .unwrap();
        }
    };

    // Round 1: scans vs drop_collection.
    seed("dropme");
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let st = Arc::clone(&st);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let hits = st.find_matching("app", "dropme", &doc! {"x": 1}).unwrap();
                    assert!(
                        hits.len() == n_docs || hits.is_empty(),
                        "partial scan racing drop_collection: {} of {} docs",
                        hits.len(),
                        n_docs
                    );
                    let n = st
                        .count_matching("app", "dropme", &doc! {"x": 1}, None)
                        .unwrap();
                    assert!(
                        n == n_docs || n == 0,
                        "partial count racing drop_collection: {n} of {n_docs}"
                    );
                }
            })
        })
        .collect();
    thread::sleep(std::time::Duration::from_millis(30));
    st.drop_collection("app", "dropme").unwrap();
    thread::sleep(std::time::Duration::from_millis(30));
    stop.store(true, Ordering::SeqCst);
    for h in readers {
        h.join().expect("reader panicked");
    }

    // Round 2: scans vs rename_collection (source empties, target fills).
    seed("src");
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let st = Arc::clone(&st);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let src = st.find_matching("app", "src", &doc! {"x": 1}).unwrap();
                    assert!(
                        src.len() == n_docs || src.is_empty(),
                        "partial scan of rename source: {} of {}",
                        src.len(),
                        n_docs
                    );
                    let dst = st.find_matching("app", "dst", &doc! {"x": 1}).unwrap();
                    assert!(
                        dst.len() == n_docs || dst.is_empty(),
                        "partial scan of rename target: {} of {}",
                        dst.len(),
                        n_docs
                    );
                }
            })
        })
        .collect();
    thread::sleep(std::time::Duration::from_millis(30));
    let (ok, err) = st
        .rename_collection("app", "src", "app", "dst", false)
        .unwrap();
    assert!(ok, "{err:?}");
    thread::sleep(std::time::Duration::from_millis(30));
    stop.store(true, Ordering::SeqCst);
    for h in readers {
        h.join().expect("reader panicked");
    }

    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
