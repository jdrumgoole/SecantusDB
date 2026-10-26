//! `WT_SESSION::compact`, `verify` and `salvage` against a real WiredTiger
//! database: a table most of whose rows were deleted gives its space back,
//! and a damaged file fails `verify` and is rebuilt by `salvage` from what
//! can still be read.

use secantus_wt::{Connection, Session};
use std::sync::atomic::{AtomicU32, Ordering};

const CONFIG: &str = "create,cache_size=64M,log=(enabled=true,file_max=10MB),\
                      transaction_sync=(enabled=false,method=fsync)";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("secantus-wt-compact-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Bytes that do not compress, so the file holds what was written.
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

fn file_size(home: &std::path::Path, file: &str) -> u64 {
    std::fs::metadata(home.join(file)).unwrap().len()
}

/// `rows` rows of 2,000 incompressible bytes, then all but every tenth
/// deleted and a checkpoint taken, so the deleted rows' blocks are free.
fn mostly_deleted(session: &Session, uri: &str, rows: i64) {
    session.create(uri, "key_format=q,value_format=u").unwrap();
    let cur = session.open_cursor(uri, None).unwrap();
    for k in 0..rows {
        cur.set_key_q(k);
        cur.set_value_u(&noise(k as u64, 2000));
        cur.insert().unwrap();
    }
    session.checkpoint(None).unwrap();
    for k in 0..rows {
        if k % 10 != 0 {
            cur.set_key_q(k);
            cur.remove().unwrap();
        }
    }
    drop(cur);
    session.checkpoint(None).unwrap();
}

#[test]
fn compact_gives_a_mostly_deleted_tables_space_back() {
    let home = temp_home();
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
    let session = conn.open_session().unwrap();
    mostly_deleted(&session, "table:big", 20_000);
    let before = file_size(&home, "big.wt");
    assert!(before > 30_000_000, "the table is {before} bytes");

    session.compact("table:big", Some("timeout=0")).unwrap();
    let after = file_size(&home, "big.wt");
    assert!(after < before / 4, "compact left {after} of {before} bytes");

    // Every remaining row reads back as written.
    let cur = session.open_cursor("table:big", None).unwrap();
    let mut seen = 0;
    while cur.next().unwrap() {
        let k = cur.get_key_q().unwrap();
        assert_eq!(k % 10, 0);
        assert_eq!(cur.get_value_u().unwrap(), noise(k as u64, 2000));
        seen += 1;
    }
    assert_eq!(seen, 2_000);
    drop(cur);

    // A second compact has nothing to give back, and is not an error.
    session.compact("table:big", None).unwrap();
    assert!(file_size(&home, "big.wt") <= after);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn compact_leaves_a_small_table_alone_and_refuses_what_it_cannot_do() {
    let home = temp_home();
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
    let session = conn.open_session().unwrap();
    session
        .create("table:small", "key_format=q,value_format=u")
        .unwrap();
    let cur = session.open_cursor("table:small", None).unwrap();
    for k in 0..10 {
        cur.set_key_q(k);
        cur.set_value_u(b"x");
        cur.insert().unwrap();
    }
    drop(cur);
    session.checkpoint(None).unwrap();
    // Too small to compact (1 MB or less): nothing is moved. The file can
    // still change size, by the checkpoint compaction takes first.
    session.compact("table:small", None).unwrap();
    assert!(file_size(&home, "small.wt") < 1 << 20);
    let cur = session.open_cursor("table:small", None).unwrap();
    let mut rows = 0;
    while cur.next().unwrap() {
        rows += 1;
    }
    assert_eq!(rows, 10);
    drop(cur);

    // Inside a transaction compact is refused (it checkpoints).
    session.begin_transaction(None).unwrap();
    assert!(session.compact("table:small", None).is_err());
    session.rollback_transaction(None).unwrap();

    // A table that does not exist is an error, not a silent success.
    assert!(session.compact("table:nope", None).is_err());
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn compact_runs_while_another_session_writes() {
    let home = temp_home();
    let conn = std::sync::Arc::new(Connection::open(home.to_str().unwrap(), CONFIG).unwrap());
    {
        let session = conn.open_session().unwrap();
        mostly_deleted(&session, "table:busy", 20_000);
    }
    let before = file_size(&home, "busy.wt");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let conn = conn.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let session = conn.open_session().unwrap();
            let cur = session.open_cursor("table:busy", None).unwrap();
            let mut k = 1_000_000i64;
            while !stop.load(Ordering::Acquire) {
                cur.set_key_q(k);
                cur.set_value_u(&noise(k as u64, 500));
                cur.insert().unwrap();
                k += 1;
            }
            k - 1_000_000
        })
    };
    let session = conn.open_session().unwrap();
    session.compact("table:busy", Some("timeout=0")).unwrap();
    stop.store(true, Ordering::Release);
    let written = writer.join().unwrap();
    assert!(written > 0);
    assert!(file_size(&home, "busy.wt") < before);

    // The writer's rows and the survivors are all there.
    let cur = session.open_cursor("table:busy", None).unwrap();
    let (mut old, mut new) = (0i64, 0i64);
    while cur.next().unwrap() {
        let k = cur.get_key_q().unwrap();
        if k >= 1_000_000 {
            assert_eq!(cur.get_value_u().unwrap(), noise(k as u64, 500));
            new += 1;
        } else {
            assert_eq!(cur.get_value_u().unwrap(), noise(k as u64, 2000));
            old += 1;
        }
    }
    assert_eq!((old, new), (2_000, written));
    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn salvage_rebuilds_a_damaged_table_from_what_reads_back() {
    let home = temp_home();
    {
        let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
        let session = conn.open_session().unwrap();
        session
            .create("table:hurt", "key_format=q,value_format=u")
            .unwrap();
        let cur = session.open_cursor("table:hurt", None).unwrap();
        for k in 0..5_000 {
            cur.set_key_q(k);
            cur.set_value_u(&noise(k as u64, 1000));
            cur.insert().unwrap();
        }
        drop(cur);
        session.checkpoint(None).unwrap();
    }
    {
        // Healthy: verify passes. On a fresh connection, as a repair runs
        // it: a table this connection has just used can still have its
        // handle open, and verify then answers EBUSY.
        let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
        let session = conn.open_session().unwrap();
        session.verify("table:hurt", None).unwrap();
    }
    // Overwrite a stretch of blocks in the middle of the file.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(home.join("hurt.wt"))
            .unwrap();
        f.seek(SeekFrom::Start(64 * 1024)).unwrap();
        f.write_all(&[0xde; 16 * 1024]).unwrap();
        f.sync_all().unwrap();
    }
    let conn = Connection::open(home.to_str().unwrap(), CONFIG).unwrap();
    let session = conn.open_session().unwrap();
    assert!(session.verify("table:hurt", None).is_err());
    session.salvage("table:hurt", None).unwrap();
    session.verify("table:hurt", None).unwrap();
    let cur = session.open_cursor("table:hurt", None).unwrap();
    let mut kept = 0;
    while cur.next().unwrap() {
        let k = cur.get_key_q().unwrap();
        assert_eq!(cur.get_value_u().unwrap(), noise(k as u64, 1000));
        kept += 1;
    }
    assert!(kept > 4_000 && kept < 5_000, "salvage kept {kept} rows");
    drop(cur);
    drop(session);
    drop(conn);
    let _ = std::fs::remove_dir_all(&home);
}
