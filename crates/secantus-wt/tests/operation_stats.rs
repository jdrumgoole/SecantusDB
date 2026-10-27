//! An operation's WiredTiger statistics (RM-023): what each session it used
//! counted, read from `statistics:session` as mongod reads its operation's
//! session for the `storage` section of a `Slow query` line.

use secantus_wt::{operation_stats_armed, operation_stats_scope, Connection, OperationStats};
use std::sync::atomic::{AtomicU32, Ordering};

/// A small cache, so a large write drafts the writing thread into eviction,
/// and statistics on, as the stores open them.
const CONFIG: &str = "create,cache_size=1M,eviction_dirty_target=20,eviction_dirty_trigger=40,\
                      log=(enabled=false),statistics=(fast)";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("secantus-wt-opstats-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Bytes that do not compress.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn write_rows(conn: &Connection, rows: u64) {
    let s = conn.open_session().unwrap();
    let c = s.open_cursor("table:t", None).unwrap();
    for i in 0..rows {
        c.set_key_q(i as i64);
        c.set_value_u(&noise(i, 4096));
        c.insert().unwrap();
    }
}

#[test]
fn a_session_closed_in_an_operation_adds_its_statistics() {
    let home = temp_home();
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
    {
        let s = conn.open_session().unwrap();
        s.create("table:t", "key_format=q,value_format=u").unwrap();
    }
    assert!(!operation_stats_armed());
    let scope = operation_stats_scope();
    assert!(operation_stats_armed());
    // 8 MB through a 1 MB cache: the writing thread evicts, and waits.
    write_rows(&conn, 2048);
    let written = scope.stats();
    assert!(written.bytes_written > 0, "{written:?}");
    assert!(written.cache_wait_micros > 0, "{written:?}");
    drop(scope);
    assert!(!operation_stats_armed());

    // Outside a scope nothing is counted, and a new scope starts at zero.
    write_rows(&conn, 16);
    let scope = operation_stats_scope();
    assert_eq!(scope.stats(), OperationStats::default());
    // Reading back what was evicted reads it from disk.
    {
        let s = conn.open_session().unwrap();
        let c = s.open_cursor("table:t", None).unwrap();
        let mut n = 0;
        while c.next().unwrap() {
            n += 1;
        }
        assert_eq!(n, 2048);
    }
    let read = scope.stats();
    assert!(read.bytes_read > 0, "{read:?}");
    drop(scope);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_long_lived_session_gives_each_statement_its_share() {
    let home = temp_home();
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
    let s = conn.open_session().unwrap();
    s.create("table:t", "key_format=q,value_format=u").unwrap();
    // Counted before any statement: not a statement's.
    {
        let c = s.open_cursor("table:t", None).unwrap();
        for i in 0..2048u64 {
            c.set_key_q(i as i64);
            c.set_value_u(&noise(i, 4096));
            c.insert().unwrap();
        }
    }
    assert!(s.statistics(false).unwrap().bytes_written > 0);
    let scope = operation_stats_scope();
    s.discard_statistics().unwrap();
    s.collect_operation_statistics().unwrap();
    assert_eq!(scope.stats(), OperationStats::default());
    // The session's counts start over once collected.
    assert_eq!(s.statistics(false).unwrap(), OperationStats::default());
    drop(scope);
    drop(s);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}
