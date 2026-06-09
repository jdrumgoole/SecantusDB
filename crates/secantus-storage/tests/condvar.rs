//! Change-stream tailable-wait primitive tests (Phase 4 sub-phase 5e gap):
//! `wait_for_oplog` blocks until the oplog advances / a notify / a timeout, and
//! `notify_oplog_waiters` wakes a blocked waiter without advancing the oplog.
//! Exercised across threads against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-cv-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

#[test]
fn wait_wakes_on_insert() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    let captured = st.oplog_tail_seq();

    let waiter = {
        let st = Arc::clone(&st);
        std::thread::spawn(move || {
            // Generous timeout; the insert should wake us well before it.
            let started = Instant::now();
            let tail = st.wait_for_oplog(captured, 10_000);
            (tail, started.elapsed())
        })
    };

    // Give the waiter time to block, then write — which emits an oplog "i".
    std::thread::sleep(Duration::from_millis(150));
    st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();

    let (tail, elapsed) = waiter.join().unwrap();
    assert!(
        tail > captured,
        "tail {tail} should exceed captured {captured}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "should wake on the insert, not time out"
    );

    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn wait_times_out_when_idle() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let tail = st.oplog_tail_seq();
    let started = Instant::now();
    // No writes; the call should return ~after the timeout with the same tail.
    let got = st.wait_for_oplog(tail, 200);
    let elapsed = started.elapsed();
    assert_eq!(got, tail);
    assert!(
        elapsed >= Duration::from_millis(150),
        "should have waited the timeout"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "should not block far past the timeout"
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn notify_wakes_waiter_without_advancing() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    let captured = st.oplog_tail_seq();

    let waiter = {
        let st = Arc::clone(&st);
        std::thread::spawn(move || {
            let started = Instant::now();
            let tail = st.wait_for_oplog(captured, 10_000);
            (tail, started.elapsed())
        })
    };

    std::thread::sleep(Duration::from_millis(150));
    st.notify_oplog_waiters(); // e.g. killCursors — wake without a new entry

    let (tail, elapsed) = waiter.join().unwrap();
    // Tail unchanged (no write), but the waiter returned promptly on the notify.
    assert_eq!(tail, captured);
    assert!(
        elapsed < Duration::from_secs(5),
        "notify should wake the waiter promptly"
    );

    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn wait_returns_immediately_when_already_advanced() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let before = st.oplog_tail_seq();
    st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
    // The tail already moved past `before`, so a wait against `before` returns
    // at once (no blocking) with the advanced tail.
    let started = Instant::now();
    let tail = st.wait_for_oplog(before, 10_000);
    assert!(tail > before);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
