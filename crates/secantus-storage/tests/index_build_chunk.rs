//! Chunked index builds (RM-187): building an index whose entries exceed
//! the cache's dirty budget must not run as one WiredTiger transaction. In
//! one transaction the build got a cache-pressure rollback that the
//! write-conflict loop re-ran forever -- at `--cache-size 8M`, 5,000
//! 300-byte documents never finished. Against real WiredTiger with a
//! deliberately small cache, as `drop_chunk.rs` does for drops.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("secantus-indexchunk-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// Abort the process if the guarded section runs past the deadline: a
/// wedged build must fail the suite loudly, not hang it.
fn watchdog(secs: u64, label: &'static str) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if rx
            .recv_timeout(std::time::Duration::from_secs(secs))
            .is_err()
        {
            eprintln!("WATCHDOG: {label} exceeded {secs}s -- wedged; aborting");
            std::process::exit(101);
        }
    });
    tx
}

#[test]
fn an_index_build_survives_a_small_cache() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("32M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let pad = "p".repeat(290);
    for batch in 0..50i64 {
        let docs: Vec<Vec<u8>> = (0..1_000i64)
            .map(|i| {
                let n = batch * 1_000 + i;
                enc(&doc! {"_id": n, "s": format!("{n:09}{pad}")})
            })
            .collect();
        st.insert("app", "c", docs, true).unwrap();
    }
    let done = watchdog(120, "an index build over 50k documents under a 32M cache");
    assert!(st
        .create_index("app", "c", "s_1", &doc! {"s": 1i32}, &Document::new())
        .unwrap());
    // Dropped and built again: the second build starts from no entries.
    assert!(st.drop_index("app", "c", "s_1").unwrap());
    assert!(st
        .create_index("app", "c", "s_1", &doc! {"s": 1i32}, &Document::new())
        .unwrap());
    let _ = done.send(());
    // Every document is reachable through the index, once.
    let found = st
        .find_matching("app", "c", &doc! {"s": {"$gte": ""}})
        .unwrap();
    assert_eq!(found.len(), 50_000);
    let one = st
        .find_matching("app", "c", &doc! {"s": format!("{:09}{pad}", 31_337)})
        .unwrap();
    assert_eq!(one.len(), 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
