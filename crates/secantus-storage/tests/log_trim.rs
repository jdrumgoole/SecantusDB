//! A logged, durable store trims its log while it is open.
//!
//! WiredTiger removes a log file only after a checkpoint, and until
//! 2026-10-13 a logged store checkpointed only when it closed. A server that
//! ran for 12 minutes at 13,000 updates a second held 2.0 GB of log over
//! tables of a few kilobytes, and took 38 s to recover from a SIGKILL.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use secantus_storage::{wt_config, Storage, StorageOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-logtrim-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn log_files(home: &Path) -> usize {
    std::fs::read_dir(home)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("WiredTigerLog.")
        })
        .count()
}

/// Write until the log has rolled over into several files.
fn fill_log(st: &Storage, home: &Path, at_least: usize) {
    let pad = "x".repeat(4096);
    let mut i = 0i64;
    while log_files(home) < at_least {
        let docs = (0..50)
            .map(|_| {
                i += 1;
                bson::to_vec(&bson::doc! {"_id": i, "pad": &pad}).unwrap()
            })
            .collect();
        let (n, errors) = st.insert("db", "c", docs, true).unwrap();
        assert_eq!((n, errors.len()), (50, 0));
        assert!(i < 200_000, "the log never rolled over");
    }
}

fn open(home: &Path, durable: bool, checkpoint_seconds: u64) -> Storage {
    Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            // The smallest log file WiredTiger allows, so a test fills several.
            wt_config: Some(wt_config("64M", 100, false, "100KB")),
            durable: Some(durable),
            data_nonlogged: Some(false),
            checkpoint_seconds: Some(checkpoint_seconds),
            ..StorageOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn a_durable_logged_store_removes_log_files_while_it_is_open() {
    let home = temp_home();
    let st = open(&home, true, 1);
    fill_log(&st, &home, 8);
    let before = log_files(&home);
    // The thread checkpoints once a second; a checkpoint lets WiredTiger
    // remove every log file before it.
    let deadline = Instant::now() + Duration::from_secs(30);
    while log_files(&home) >= before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let after = log_files(&home);
    assert!(
        after < before,
        "{before} log files and still {after} after 30 s: the open store never checkpointed"
    );
    // Every document is still there, and still there after a reopen.
    let count = st
        .count_matching("db", "c", &bson::Document::new(), None)
        .unwrap();
    assert!(count >= 50);
    drop(st);
    let st = open(&home, true, 1);
    assert_eq!(
        st.count_matching("db", "c", &bson::Document::new(), None)
            .unwrap(),
        count
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_fast_test_store_runs_no_checkpoint_thread() {
    // `durable=false` is the test suite's mode: a store that lives for
    // seconds and skips its close checkpoint must not start syncing one a
    // second either.
    let home = temp_home();
    let st = open(&home, false, 1);
    fill_log(&st, &home, 8);
    let before = log_files(&home);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        log_files(&home) >= before,
        "a durable=false store checkpointed while open"
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
