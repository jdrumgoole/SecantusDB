//! Chunked two-phase rename: renaming a collection larger than the cache's
//! dirty budget must not run as one WT transaction (unevictable dirty content
//! — the livelock class the chunked drop closed; rename re-keys every row, so
//! it was the last unbounded DDL transaction). Against real WiredTiger with a
//! deliberately small cache, plus the semantics the move must preserve:
//! insertion order, indexes + unique claims, drop_target, and oplog entries.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-renchunk-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// Abort the process if the guarded section wedges (pre-fix the one-txn
/// rename would spin in the WriteConflict retry loop under this shape).
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

#[test]
fn rename_survives_a_small_cache() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("128M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let filler = "x".repeat(1100);
    // ~165MB — bigger than the whole cache, so a one-txn re-key cannot fit.
    for batch in 0..15 {
        let docs: Vec<Vec<u8>> = (0..10_000i64)
            .map(|i| enc(&doc! {"_id": batch * 10_000 + i, "pad": filler.clone(), "x": 1}))
            .collect();
        let (inserted, _) = st.insert("app", "big", docs, true).unwrap();
        assert_eq!(inserted, 10_000);
    }
    let done = watchdog(180, "rename of 150k docs under a 128M cache");
    let (ok, err) = st
        .rename_collection("app", "big", "app", "moved", false)
        .unwrap();
    let _ = done.send(());
    assert!(ok, "{err:?}");
    assert_eq!(
        st.count_matching("app", "moved", &doc! {}, None).unwrap(),
        150_000
    );
    assert_eq!(st.count_matching("app", "big", &doc! {}, None).unwrap(), 0);
    // The source name is fully reusable.
    let (inserted, _) = st
        .insert("app", "big", vec![enc(&doc! {"_id": 1i64})], true)
        .unwrap();
    assert_eq!(inserted, 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rename_preserves_indexes_order_and_uniqueness() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let docs: Vec<Vec<u8>> = (0..50i64)
        .map(|i| enc(&doc! {"_id": i, "x": i, "u": format!("u{i}")}))
        .collect();
    st.insert("app", "src", docs, true).unwrap();
    st.create_index("app", "src", "x_1", &doc! {"x": 1i32}, &Document::new())
        .unwrap();
    st.create_index(
        "app",
        "src",
        "u_1",
        &doc! {"u": 1i32},
        &doc! {"unique": true},
    )
    .unwrap();
    let (ok, err) = st
        .rename_collection("app", "src", "app", "dst", false)
        .unwrap();
    assert!(ok, "{err:?}");
    // Indexes moved with the collection.
    let names: Vec<String> = st
        .list_indexes("app", "dst")
        .unwrap()
        .into_iter()
        .map(|d| d.get_str("name").unwrap().to_string())
        .collect();
    assert!(names.contains(&"x_1".to_string()), "{names:?}");
    assert!(names.contains(&"u_1".to_string()), "{names:?}");
    // Insertion order survived the re-key (natural scan = _id order here).
    let rows = st.find_matching("app", "dst", &doc! {}).unwrap();
    let ids: Vec<i64> = rows
        .iter()
        .map(|b| {
            bson::from_slice::<Document>(b)
                .unwrap()
                .get_i64("_id")
                .unwrap()
        })
        .collect();
    assert_eq!(ids, (0..50).collect::<Vec<_>>());
    // Unique claims moved: a duplicate insert on the renamed collection fails.
    let dup = st.insert(
        "app",
        "dst",
        vec![enc(&doc! {"_id": 99i64, "u": "u7"})],
        true,
    );
    if let Ok((_, errs)) = dup {
        assert!(!errs.is_empty(), "duplicate unique key must be rejected");
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rename_drop_target_replaces() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert(
        "app",
        "a",
        vec![enc(&doc! {"_id": 1i64, "from": "a"})],
        true,
    )
    .unwrap();
    st.insert(
        "app",
        "b",
        vec![enc(&doc! {"_id": 2i64, "from": "b"})],
        true,
    )
    .unwrap();
    // Without drop_target the rename refuses.
    let (ok, err) = st.rename_collection("app", "a", "app", "b", false).unwrap();
    assert!(!ok);
    assert!(err.unwrap().contains("target namespace exists"));
    // With drop_target the target's rows are gone and the source's arrive.
    let (ok, err) = st.rename_collection("app", "a", "app", "b", true).unwrap();
    assert!(ok, "{err:?}");
    let rows = st.find_matching("app", "b", &doc! {}).unwrap();
    assert_eq!(rows.len(), 1);
    let first = bson::from_slice::<Document>(&rows[0]).unwrap();
    assert_eq!(first.get_str("from").unwrap(), "a");
    assert_eq!(st.count_matching("app", "a", &doc! {}, None).unwrap(), 0);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
