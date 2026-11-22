//! A collection drop is catalog-only, as mongod's is: dropping a collection
//! whose row volume exceeds the cache's dirty budget must not purge its rows
//! (the 2026-08-11 concurrency sweep wedged a live server for 1.5h when a
//! one-transaction purge blew the dirty trigger and the WriteConflict retry
//! loop re-ran it forever). The registry rows, the placement and the
//! `_mdb_catalog` row go in one small transaction with the drop's oplog
//! entry, and the tables go to the drop-pending reaper, so a kill between
//! the commit and the reaper leaves tables the next open drops and no
//! registry row. Against real WiredTiger with a deliberately small cache.

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

/// A process killed with SIGKILL right after a drop committed, before the
/// drop-pending reaper has dropped the tables: the next open finds the
/// namespace gone, no orphan index registry row, and a re-created
/// collection of the name holds only its own documents and indexes.
#[test]
fn a_kill_after_a_drop_commits_leaves_no_registry_row() {
    const CHILD: &str = "SECANTUS_DROP_KILL_CHILD";
    let open = |home: &std::path::Path| {
        Storage::open_with_options(
            home.to_str().unwrap(),
            &secantus_storage::StorageOptions {
                durable: Some(true),
                ..secantus_storage::StorageOptions::default()
            },
        )
        .unwrap()
    };
    if let Ok(home) = std::env::var(CHILD) {
        let st = open(std::path::Path::new(&home));
        let docs: Vec<Vec<u8>> = (0..50i64).map(|i| enc(&doc! {"_id": i, "x": i})).collect();
        st.insert("app", "c", docs, true).unwrap();
        st.create_index("app", "c", "x_1", &doc! {"x": 1i32}, &Document::new())
            .unwrap();
        st.insert("app", "keep", vec![enc(&doc! {"_id": 1i64})], true)
            .unwrap();
        assert!(st.drop_collection("app", "c").unwrap());
        // The commit reaches the journal; no checkpoint, so the reaper has not
        // dropped a table.
        st.flush_journal().unwrap();
        // SIGKILL: no destructor, no close, no reaper.
        let _ = std::process::Command::new("kill")
            .args(["-9", &std::process::id().to_string()])
            .status();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    let home = temp_home();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_kill_after_a_drop_commits_leaves_no_registry_row",
            "--test-threads=1",
        ])
        .env(CHILD, home.to_str().unwrap())
        .status()
        .unwrap();
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "the child was not killed: {status:?}"
        );
    }
    let st = open(&home);
    let names = st.list_collections("app").unwrap();
    assert!(!names.contains(&"c".to_string()), "{names:?}");
    assert!(names.contains(&"keep".to_string()), "{names:?}");
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 0);
    // An orphan registry row would list the index again under the name.
    st.insert("app", "c", vec![enc(&doc! {"_id": 100i64})], true)
        .unwrap();
    let indexes: Vec<String> = st
        .list_indexes("app", "c")
        .unwrap()
        .into_iter()
        .map(|d| d.get_str("name").unwrap().to_string())
        .collect();
    assert_eq!(indexes, vec!["_id_".to_string()], "{indexes:?}");
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
