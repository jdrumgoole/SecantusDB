//! RM-736 (decided by Joe, 2026-11-22: "Truncate as mongod"): a store whose
//! data tables are not logged truncates its oplog after
//! `local.replset.oplogTruncateAfterPoint` as it opens after a crash, then
//! clears the point, before it replays the oplog from the stable timestamp:
//! mongod's `ReplicationRecovery::_truncateOplogIfNeededAndThenClearOplog
//! TruncateAfterPoint`. Each crash is a child process that ends without
//! closing the store, its commits synced (`transaction_sync`), with the
//! journal flusher paused (mongod's `pauseJournalFlusherBeforeFlush`) so the
//! point stays where the test left it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageOptions};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-otap-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `secantusd-mdb`'s store (unlogged data over a logged oplog), every
/// commit synced: what a commit acknowledged is in the journal, so only the
/// truncation can take it.
fn options(checkpoint_seconds: Option<u64>) -> StorageOptions {
    StorageOptions {
        wt_config: Some(secantus_storage::wt_config("256M", 100, true, "128MB")),
        durable: Some(true),
        data_nonlogged: Some(true),
        oplog_nonlogged: Some(false),
        checkpoint_seconds,
        ..StorageOptions::default()
    }
}

fn open(home: &Path, opts: &StorageOptions) -> Storage {
    Storage::open_with_options(home.to_str().unwrap(), opts).unwrap()
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn has(st: &Storage, id: i32) -> bool {
    st.find_by_id("app", "c", &Bson::Int32(id))
        .unwrap()
        .is_some()
}

fn point(st: &Storage) -> Bson {
    let rows = st
        .find_matching(
            "local",
            "replset.oplogTruncateAfterPoint",
            &doc! {"_id": "oplogTruncateAfterPoint"},
        )
        .unwrap();
    bson::from_slice::<Document>(&rows[0])
        .unwrap()
        .get("oplogTruncateAfterPoint")
        .cloned()
        .unwrap()
}

fn entry_ids(st: &Storage) -> Vec<Bson> {
    st.read_oplog(0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, blob)| {
            let e: Document = bson::from_slice(&blob).ok()?;
            (e.get_str("ns") == Ok("app.c") && e.get_str("op") == Ok("i"))
                .then(|| e.get_document("o").ok()?.get("_id").cloned())
                .flatten()
        })
        .collect()
}

/// Run `test` again as a child with `child_var` naming the store, and wait
/// for it: the child writes and ends without closing.
fn crash_child(test: &str, child_var: &str, home: &Path) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--test-threads=1"])
        .env(child_var, home.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success(), "the child failed");
}

/// The entries committed after the point last moved are truncated, their
/// writes with them; the point is cleared; the entries at or below it are
/// replayed. A journaled write (its wait moves the point) survives.
#[test]
fn recovery_truncates_after_the_point_and_clears_it() {
    const CHILD: &str = "SECANTUS_OTAP_CHILD_TRUNCATE";
    let opts = options(None);
    if let Ok(home) = std::env::var(CHILD) {
        let st = open(Path::new(&home), &opts);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        // Journaled: the point moves to its entry first.
        st.flush_journal().unwrap();
        st.pause_journal_flusher(true);
        // A flusher round already past its check ends before the writes.
        std::thread::sleep(std::time::Duration::from_millis(200));
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3})).unwrap();
        st.flush_oplog();
        std::process::exit(0);
    }
    let home = temp_home();
    crash_child(
        "recovery_truncates_after_the_point_and_clears_it",
        CHILD,
        &home,
    );
    let st = open(&home, &opts);
    let truncation = st
        .open_record()
        .oplog_truncation
        .expect("the point was set: the oplog was truncated");
    assert_eq!(truncation.removed, 2);
    assert_eq!(entry_ids(&st), vec![Bson::Int32(1)]);
    assert_eq!(
        (has(&st, 1), has(&st, 2), has(&st, 3)),
        (true, false, false)
    );
    // Cleared by the recovery; moved again by the flusher once it runs.
    drop(st);
    let st = open(&home, &opts);
    assert_eq!(st.open_record().oplog_truncation, None);
    assert_eq!(
        point(&st),
        Bson::Timestamp(bson::Timestamp {
            time: 0,
            increment: 0
        })
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A point at or below the stable timestamp truncates after the stable
/// timestamp instead: what the stable checkpoint holds stays.
#[test]
fn a_point_below_the_stable_timestamp_truncates_after_the_stable_timestamp() {
    const CHILD: &str = "SECANTUS_OTAP_CHILD_STABLE";
    // A stable checkpoint every second (the periodic one moves no point).
    let opts = options(Some(1));
    if let Ok(home) = std::env::var(CHILD) {
        let st = open(Path::new(&home), &opts);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        st.flush_journal().unwrap();
        st.pause_journal_flusher(true);
        // A flusher round already past its check ends before the writes.
        std::thread::sleep(std::time::Duration::from_millis(200));
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        st.flush_oplog();
        // The periodic stable checkpoint takes the second insert.
        let before = st.stable_checkpoint_seq();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while st.stable_checkpoint_seq() == before {
            assert!(std::time::Instant::now() < deadline, "no stable checkpoint");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        st.insert_one("app", "c", &enc(&doc! {"_id": 3})).unwrap();
        st.flush_oplog();
        std::process::exit(0);
    }
    let home = temp_home();
    crash_child(
        "a_point_below_the_stable_timestamp_truncates_after_the_stable_timestamp",
        CHILD,
        &home,
    );
    let st = open(&home, &opts);
    let truncation = st.open_record().oplog_truncation.expect("truncated");
    assert!(truncation.after > truncation.point, "{truncation:?}");
    assert_eq!(truncation.after, st.open_record().recovery_timestamp);
    assert_eq!(truncation.removed, 1);
    assert_eq!((has(&st, 1), has(&st, 2), has(&st, 3)), (true, true, false));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A clean close leaves `Timestamp(0, 0)`, and an open truncates nothing;
/// a crash with the point at the oplog's last entry truncates nothing.
#[test]
fn a_cleared_or_current_point_truncates_nothing() {
    const CHILD: &str = "SECANTUS_OTAP_CHILD_CURRENT";
    let opts = options(None);
    if let Ok(home) = std::env::var(CHILD) {
        let st = open(Path::new(&home), &opts);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        st.flush_journal().unwrap();
        std::process::exit(0);
    }
    let home = temp_home();
    {
        let st = open(&home, &opts);
        st.insert_one("app", "c", &enc(&doc! {"_id": 0})).unwrap();
    }
    let st = open(&home, &opts);
    assert_eq!(st.open_record().oplog_truncation, None);
    drop(st);
    crash_child("a_cleared_or_current_point_truncates_nothing", CHILD, &home);
    let st = open(&home, &opts);
    let truncation = st
        .open_record()
        .oplog_truncation
        .expect("the point was set");
    assert_eq!(truncation.removed, 0);
    assert_eq!((has(&st, 0), has(&st, 1), has(&st, 2)), (true, true, true));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
