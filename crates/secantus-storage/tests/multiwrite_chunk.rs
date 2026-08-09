//! Chunked multi-document write transactions: updateMany / deleteMany over a
//! large matched set must not run as one WT transaction (unevictable dirty
//! content — the livelock class of the chunked-insert work), and chunking
//! must keep exactly-once transform semantics. Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-mwchunk-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// A whole-collection rewrite of ~39MB of documents against a deliberately
/// small cache: pre-chunking this was one unbounded statement transaction
/// (the wedge class); chunked it completes quickly, and a deleteMany of the
/// same set follows suit.
#[test]
fn update_and_delete_many_survive_a_small_cache() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("128M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let filler = "x".repeat(1100);
    let docs: Vec<Vec<u8>> = (0..35_000i64)
        .map(|i| enc(&doc! {"_id": i, "pad": filler.clone(), "x": 1}))
        .collect();
    let (inserted, _) = st.insert("app", "c", docs, true).unwrap();
    assert_eq!(inserted, 35_000);
    let out = st
        .update_matching(
            "app",
            "c",
            &doc! {"x": 1},
            &doc! {"$set": {"x": 2}},
            true,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
    assert_eq!(out.matched, 35_000);
    assert_eq!(out.modified, 35_000);
    assert_eq!(
        st.count_matching("app", "c", &doc! {"x": 2}, None).unwrap(),
        35_000
    );
    let deleted = st
        .delete_matching("app", "c", &doc! {"x": 2}, 0, &Document::new(), None)
        .unwrap();
    assert_eq!(deleted, 35_000);
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 0);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// $inc across chunk boundaries applies exactly once per document — the
/// RecordId list is partitioned across chunks and a conflict retry re-runs
/// only its own rolled-back chunk.
#[test]
fn multi_update_inc_applies_exactly_once_across_chunks() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    // > WRITE_CHUNK_MAX_DOCS so at least two chunks run.
    let docs: Vec<Vec<u8>> = (0..2500i64)
        .map(|i| enc(&doc! {"_id": i, "n": 0}))
        .collect();
    st.insert("app", "c", docs, true).unwrap();
    let out = st
        .update_matching(
            "app",
            "c",
            &doc! {},
            &doc! {"$inc": {"n": 1}},
            true,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
    assert_eq!(out.matched, 2500);
    assert_eq!(out.modified, 2500);
    assert_eq!(
        st.count_matching("app", "c", &doc! {"n": 1}, None).unwrap(),
        2500,
        "every doc incremented exactly once"
    );
    // The filter is re-checked inside each chunk: docs the first pass
    // updated out of a match (x flips) are not double-processed.
    let out = st
        .update_matching(
            "app",
            "c",
            &doc! {"n": 1},
            &doc! {"$inc": {"n": 1}},
            true,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
    assert_eq!(out.modified, 2500);
    assert_eq!(
        st.count_matching("app", "c", &doc! {"n": 2}, None).unwrap(),
        2500
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// deleteOne (limit=1) and single-doc updates stay on the single-transaction
/// path and behave as before.
#[test]
fn bounded_paths_unchanged() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let docs: Vec<Vec<u8>> = (0..10i64).map(|i| enc(&doc! {"_id": i, "x": 1})).collect();
    st.insert("app", "c", docs, true).unwrap();
    let out = st
        .update_matching(
            "app",
            "c",
            &doc! {"x": 1},
            &doc! {"$set": {"x": 9}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            true,
        )
        .unwrap();
    assert_eq!((out.matched, out.modified), (1, 1));
    assert!(out.post_image.is_some());
    assert_eq!(
        st.delete_matching("app", "c", &doc! {"x": 1}, 1, &Document::new(), None)
            .unwrap(),
        1
    );
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 9);
    // Upsert through the chunked route's zero-match delegation.
    let out = st
        .update_matching(
            "app",
            "c",
            &doc! {"x": 777},
            &doc! {"$set": {"y": 1}},
            true,
            true,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
    assert_eq!(out.matched, 0);
    assert!(out.upserted_id.is_some());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
