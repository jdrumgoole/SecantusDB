//! Integration tests for `secantus-wt` against a real WiredTiger database.
//!
//! These exercise the actual vendored WiredTiger C library (resolved by
//! `build.rs`), so they need it present — they run wherever the crate builds.

use secantus_wt::{Connection, Session};
use std::sync::atomic::{AtomicU32, Ordering};

const CONFIG: &str = "create,cache_size=128M,log=(enabled=true,file_max=10MB),\
                      transaction_sync=(enabled=false,method=fsync)";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A throwaway database directory under the target dir, unique per test.
fn temp_home() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-wt-test-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(home: &std::path::Path) -> (Connection, Session) {
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).expect("open");
    let session = conn.open_session().expect("session");
    (conn, session)
}

#[test]
fn documents_table_ssu_natural_order_scan() {
    let home = temp_home();
    let (conn, session) = open(&home);
    session
        .create("table:docs", "key_format=SSu,value_format=u")
        .unwrap();
    let cur = session.open_cursor("table:docs", None).unwrap();

    // (db, coll, id_key) -> bson-ish value. Insert out of id_key order.
    let rows: [(&str, &str, &[u8], &[u8]); 3] = [
        ("app", "users", b"\x03", b"carol"),
        ("app", "users", b"\x01", b"alice"),
        ("app", "users", b"\x02", b"bob"),
    ];
    for (db, coll, id, val) in rows {
        cur.set_key_ssu(db, coll, id);
        cur.set_value_u(val);
        cur.insert().unwrap();
    }

    // Natural-order scan == sorted by (db, coll, id_key) bytes.
    cur.reset().unwrap();
    let mut seen = Vec::new();
    while cur.next().unwrap() {
        let (db, coll, id) = cur.get_key_ssu().unwrap();
        let val = cur.get_value_u().unwrap();
        seen.push((db, coll, id, String::from_utf8(val).unwrap()));
    }
    assert_eq!(
        seen,
        vec![
            ("app".into(), "users".into(), vec![1u8], "alice".into()),
            ("app".into(), "users".into(), vec![2u8], "bob".into()),
            ("app".into(), "users".into(), vec![3u8], "carol".into()),
        ]
    );

    // Point search by full key.
    cur.set_key_ssu("app", "users", b"\x02");
    cur.search().unwrap();
    assert_eq!(cur.get_value_u().unwrap(), b"bob");

    // Missing key -> WT_NOTFOUND.
    cur.set_key_ssu("app", "users", b"\x09");
    let err = cur.search().unwrap_err();
    assert!(err.is_not_found(), "expected NOTFOUND, got {err}");

    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn update_and_remove() {
    let home = temp_home();
    let (conn, session) = open(&home);
    session
        .create("table:t", "key_format=u,value_format=u")
        .unwrap();
    let cur = session.open_cursor("table:t", None).unwrap();

    cur.set_key_u(b"k");
    cur.set_value_u(b"v1");
    cur.insert().unwrap();

    cur.set_key_u(b"k");
    cur.set_value_u(b"v2");
    cur.update().unwrap();
    cur.set_key_u(b"k");
    cur.search().unwrap();
    assert_eq!(cur.get_value_u().unwrap(), b"v2");

    cur.set_key_u(b"k");
    cur.remove().unwrap();
    cur.set_key_u(b"k");
    assert!(cur.search().unwrap_err().is_not_found());

    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn oplog_q_keys_sort_numerically() {
    let home = temp_home();
    let (conn, session) = open(&home);
    session
        .create("table:oplog", "key_format=q,value_format=u")
        .unwrap();
    let cur = session.open_cursor("table:oplog", None).unwrap();

    for seq in [10i64, 2, 100, 3] {
        cur.set_key_q(seq);
        cur.set_value_u(format!("entry-{seq}").as_bytes());
        cur.insert().unwrap();
    }
    cur.reset().unwrap();
    let mut seqs = Vec::new();
    while cur.next().unwrap() {
        seqs.push(cur.get_key_q().unwrap());
    }
    assert_eq!(seqs, vec![2, 3, 10, 100], "q keys sort numerically");

    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn transaction_commit_and_rollback() {
    let home = temp_home();
    let (conn, session) = open(&home);
    session
        .create("table:t", "key_format=u,value_format=u")
        .unwrap();

    // Committed write is visible.
    session.begin_transaction(None).unwrap();
    {
        let cur = session.open_cursor("table:t", None).unwrap();
        cur.set_key_u(b"a");
        cur.set_value_u(b"1");
        cur.insert().unwrap();
    }
    session.commit_transaction(None).unwrap();

    // Rolled-back write is not.
    session.begin_transaction(None).unwrap();
    {
        let cur = session.open_cursor("table:t", None).unwrap();
        cur.set_key_u(b"b");
        cur.set_value_u(b"2");
        cur.insert().unwrap();
    }
    session.rollback_transaction(None).unwrap();

    let cur = session.open_cursor("table:t", None).unwrap();
    cur.set_key_u(b"a");
    assert!(cur.search().is_ok());
    cur.set_key_u(b"b");
    assert!(cur.search().unwrap_err().is_not_found());

    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn reopen_persists_data() {
    let home = temp_home();
    {
        let (conn, session) = open(&home);
        session
            .create("table:c", "key_format=SS,value_format=u")
            .unwrap();
        let cur = session.open_cursor("table:c", None).unwrap();
        cur.set_key_ss("app", "users");
        cur.set_value_u(b"opts");
        cur.insert().unwrap();
        session.checkpoint(None).unwrap();
        drop(cur);
        drop(session);
        drop(conn);
    }
    // Reopen the same directory; data survives.
    {
        let (conn, session) = open(&home);
        let cur = session.open_cursor("table:c", None).unwrap();
        cur.set_key_ss("app", "users");
        cur.search().unwrap();
        assert_eq!(cur.get_value_u().unwrap(), b"opts");
        let (db, coll) = {
            cur.reset().unwrap();
            assert!(cur.next().unwrap());
            cur.get_key_ss().unwrap()
        };
        assert_eq!((db.as_str(), coll.as_str()), ("app", "users"));
        drop(cur);
        drop(session);
        drop(conn);
    }
    let _ = std::fs::remove_dir_all(&home);
}
