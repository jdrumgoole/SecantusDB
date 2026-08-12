//! Chunked collection-drop transactions: dropping a collection whose row
//! volume exceeds the cache's dirty budget must not run as one WT
//! transaction (unevictable dirty content — the livelock class the chunked
//! insert / updateMany / deleteMany work closed; the 2026-08-11 concurrency
//! sweep wedged a live server for 1.5h in exactly this shape: the one-txn
//! purge blew the dirty trigger, WT returned a cache-pressure WT_ROLLBACK,
//! and the WriteConflict retry loop re-ran the same purge forever while the
//! eviction threads spun). Against real WiredTiger with a deliberately
//! small cache.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-dropchunk-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// Abort the whole process if the guarded section runs past the deadline —
/// a wedged drop must fail the suite loudly, not hang it (pre-fix this
/// repro spun indefinitely in the WriteConflict retry loop).
fn watchdog(secs: u64, label: &'static str) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if rx
            .recv_timeout(std::time::Duration::from_secs(secs))
            .is_err()
        {
            eprintln!("WATCHDOG: {label} exceeded {secs}s — wedged; aborting");
            std::process::exit(101);
        }
    });
    tx
}

/// Drop of a collection much larger than the cache: pre-chunking this was
/// one unbounded statement transaction whose delete markers blew the dirty
/// trigger (the wedge class); chunked it completes.
#[test]
fn drop_collection_survives_a_small_cache() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("128M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let filler = "x".repeat(1100);
    // ~165MB of documents — bigger than the whole 128M cache, so a one-txn
    // purge cannot fit its dirty content under any eviction strategy.
    for batch in 0..15 {
        let docs: Vec<Vec<u8>> = (0..10_000i64)
            .map(|i| enc(&doc! {"_id": batch * 10_000 + i, "pad": filler.clone(), "x": 1}))
            .collect();
        let (inserted, _) = st.insert("app", "big", docs, true).unwrap();
        assert_eq!(inserted, 10_000);
    }
    let done = watchdog(120, "drop_collection of 150k docs under a 128M cache");
    let existed = st.drop_collection("app", "big").unwrap();
    let _ = done.send(());
    assert!(existed);
    // The namespace is really gone and reusable.
    assert_eq!(st.count_matching("app", "big", &doc! {}, None).unwrap(), 0);
    let (inserted, _) = st
        .insert("app", "big", vec![enc(&doc! {"_id": 1i64})], true)
        .unwrap();
    assert_eq!(inserted, 1);
    assert_eq!(st.count_matching("app", "big", &doc! {}, None).unwrap(), 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
