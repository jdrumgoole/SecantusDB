//! Per-store `StorageOptions` (Phase C: first-class options over the
//! `SECANTUS_*` env vars). Each mode must engage from the option alone —
//! no environment involved — and an explicit option must win for that
//! store only. Against real WiredTiger.

use bson::doc;
use secantus_storage::{Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-opts-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn insert_row(st: &Storage, db: &str, coll: &str, id: i32) {
    st.insert_one(db, coll, &bson::to_vec(&doc! {"_id": id, "v": id}).unwrap())
        .unwrap();
}

/// `oplog_async: Some(true)` engages the drainer pool with no
/// `SECANTUS_OPLOG_ASYNC` in the environment: entries are minted post-commit
/// and become readable once drained (`flush_oplog` waits for that).
#[test]
fn oplog_async_option_engages_without_env() {
    assert!(
        std::env::var_os("SECANTUS_OPLOG_ASYNC").is_none(),
        "test requires a clean environment"
    );
    let home = temp_home();
    {
        let st = Storage::open_with_options(
            home.to_str().unwrap(),
            &StorageOptions {
                oplog_async: Some(true),
                ..StorageOptions::default()
            },
        )
        .unwrap();
        for i in 0..5 {
            insert_row(&st, "app", "c", i);
        }
        st.flush_oplog();
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(rows.len(), 5, "drained oplog should hold all 5 inserts");
        drop(st);
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `data_nonlogged: Some(true)` selects the log-only-the-oplog mode from the
/// option alone: the store records the mode in its stable marker, spawns the
/// checkpoint machinery, and a reopen WITHOUT the option keeps the recorded
/// mode (create-time-sticky, marker authoritative).
#[test]
fn data_nonlogged_option_engages_and_reopen_keeps_mode() {
    assert!(
        std::env::var_os("SECANTUS_DATA_NONLOGGED").is_none(),
        "test requires a clean environment"
    );
    let home = temp_home();
    {
        let st = Storage::open_with_options(
            home.to_str().unwrap(),
            &StorageOptions {
                data_nonlogged: Some(true),
                checkpoint_seconds: Some(3600), // cadence far away; anchor manually
                ..StorageOptions::default()
            },
        )
        .unwrap();
        for i in 0..10 {
            insert_row(&st, "app", "c", i);
        }
        st.stable_checkpoint().unwrap();
        assert!(
            st.stable_checkpoint_seq() > 0,
            "marker should have anchored"
        );
        drop(st);
    }
    {
        // Reopen with NO options: the recorded mode must win and the data
        // must be intact (clean close in the nonlogged mode).
        let st =
            Storage::open_with_options(home.to_str().unwrap(), &StorageOptions::default()).unwrap();
        let docs = st.scan_collection("app", "c").unwrap();
        assert_eq!(docs.len(), 10);
        assert!(
            st.stable_checkpoint_seq() > 0,
            "reopen must resolve the nonlogged mode from the marker"
        );
        drop(st);
    }
    let _ = std::fs::remove_dir_all(&home);
}
