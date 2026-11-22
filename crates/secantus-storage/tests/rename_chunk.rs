//! A rename is catalog-only, as mongod's is: the documents' table and the
//! indexes' tables stay and move to the new name with their placement, in
//! one small transaction with the oplog entry, so renaming a collection
//! larger than the cache's dirty budget moves no rows. Against real
//! WiredTiger with a deliberately small cache, plus the semantics the move
//! must preserve: insertion order, indexes + unique claims, drop_target, and
//! a kill right after the commit.

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

/// Abort the process if the guarded section wedges (a rename that moved
/// rows in one transaction would spin in the WriteConflict retry loop under
/// this shape).
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
    // ~165MB — bigger than the whole cache, so a rename that moved rows in one transaction could not fit.
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
    // Insertion order survived the rename (natural scan = _id order here).
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

/// A process killed with SIGKILL right after a rename (with a replaced
/// target) committed: the next open finds the source gone, the destination
/// holding the source's documents and indexes, the replaced target's
/// registry rows gone, and no orphan registry row under the old names.
#[test]
fn a_kill_after_a_rename_commits_leaves_no_registry_row() {
    const CHILD: &str = "SECANTUS_RENAME_KILL_CHILD";
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
        st.insert("app", "src", docs, true).unwrap();
        st.create_index("app", "src", "x_1", &doc! {"x": 1i32}, &Document::new())
            .unwrap();
        st.insert("app", "dst", vec![enc(&doc! {"_id": 1i64, "y": 1})], true)
            .unwrap();
        st.create_index("app", "dst", "y_1", &doc! {"y": 1i32}, &Document::new())
            .unwrap();
        let (ok, err) = st
            .rename_collection("app", "src", "app", "dst", true)
            .unwrap();
        assert!(ok, "{err:?}");
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
            "a_kill_after_a_rename_commits_leaves_no_registry_row",
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
    assert!(!names.contains(&"src".to_string()), "{names:?}");
    assert!(names.contains(&"dst".to_string()), "{names:?}");
    assert_eq!(st.count_matching("app", "dst", &doc! {}, None).unwrap(), 50);
    let index_names = |coll: &str| -> Vec<String> {
        let mut v: Vec<String> = st
            .list_indexes("app", coll)
            .unwrap()
            .into_iter()
            .map(|d| d.get_str("name").unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    // The replaced target's index went; the source's came.
    assert_eq!(index_names("dst"), vec!["_id_", "x_1"]);
    // An orphan registry row under the old name would list again here.
    st.insert("app", "src", vec![enc(&doc! {"_id": 7i64})], true)
        .unwrap();
    assert_eq!(index_names("src"), vec!["_id_"]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
