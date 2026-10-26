//! WiredTiger's timestamps, as the Rust storage uses them for snapshot reads
//! (RM-210): commit timestamps, reads at a timestamp, the oldest timestamp,
//! and what survives a checkpoint and reopen with no stable timestamp set.

use secantus_wt::{Connection, Session};
use std::sync::atomic::{AtomicU32, Ordering};

const CONFIG: &str = "create,cache_size=128M,log=(enabled=true,file_max=10MB),\
                      transaction_sync=(enabled=false,method=fsync)";

/// A data table as the storage creates one in data-nonlogged mode.
const TABLE: &str = "key_format=S,value_format=u,log=(enabled=false)";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-wt-ts-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(s: &Session, key: &str, value: &str, ts: Option<u64>) -> Result<(), String> {
    s.begin_transaction(None).map_err(|e| e.to_string())?;
    let cur = s.open_cursor("table:t", None).map_err(|e| e.to_string())?;
    cur.set_key_s(key);
    cur.set_value_u(value.as_bytes());
    cur.insert().map_err(|e| e.to_string())?;
    drop(cur);
    let cfg = ts.map(|t| format!("commit_timestamp={t:x}"));
    s.commit_transaction(cfg.as_deref())
        .map_err(|e| e.to_string())
}

fn get(s: &Session, key: &str, at: Option<u64>) -> Result<Option<String>, String> {
    let cfg = at.map(|t| format!("read_timestamp={t:x}"));
    s.begin_transaction(cfg.as_deref())
        .map_err(|e| e.to_string())?;
    let cur = s.open_cursor("table:t", None).map_err(|e| e.to_string())?;
    cur.set_key_s(key);
    let out = match cur.search() {
        Ok(()) => Some(String::from_utf8(cur.get_value_u().unwrap()).unwrap()),
        Err(e) if e.is_not_found() => None,
        Err(e) => {
            drop(cur);
            let _ = s.rollback_transaction(None);
            return Err(e.to_string());
        }
    };
    drop(cur);
    s.rollback_transaction(None).map_err(|e| e.to_string())?;
    Ok(out)
}

fn open(home: &std::path::Path) -> Connection {
    Connection::open(home.to_str().unwrap(), CONFIG).unwrap()
}

#[test]
fn a_read_at_a_timestamp_sees_the_writes_committed_at_or_before_it() {
    let home = temp_home();
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    s.create("table:t", TABLE).unwrap();
    put(&s, "k", "v1", Some(10)).unwrap();
    put(&s, "k", "v2", Some(20)).unwrap();
    assert_eq!(get(&s, "k", Some(9)).unwrap(), None);
    assert_eq!(get(&s, "k", Some(15)).unwrap().as_deref(), Some("v1"));
    assert_eq!(get(&s, "k", Some(20)).unwrap().as_deref(), Some("v2"));
    assert_eq!(get(&s, "k", None).unwrap().as_deref(), Some("v2"));
    // A write with no timestamp is visible at every timestamp.
    put(&s, "u", "plain", None).unwrap();
    assert_eq!(get(&s, "u", Some(1)).unwrap().as_deref(), Some("plain"));
}

#[test]
fn history_survives_a_checkpoint_and_reopen_without_a_stable_timestamp() {
    let home = temp_home();
    {
        let conn = open(&home);
        let s = conn.open_session().unwrap();
        s.create("table:t", TABLE).unwrap();
        put(&s, "k", "v1", Some(10)).unwrap();
        put(&s, "k", "v2", Some(20)).unwrap();
        s.checkpoint(None).unwrap();
        put(&s, "k", "v3", Some(30)).unwrap();
    }
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    assert_eq!(get(&s, "k", None).unwrap().as_deref(), Some("v3"));
    assert_eq!(get(&s, "k", Some(15)).unwrap().as_deref(), Some("v1"));
    assert_eq!(get(&s, "k", Some(25)).unwrap().as_deref(), Some("v2"));
}

#[test]
fn the_oldest_timestamp_bounds_reads_and_commits() {
    let home = temp_home();
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    s.create("table:t", TABLE).unwrap();
    put(&s, "k", "v1", Some(10)).unwrap();
    put(&s, "k", "v2", Some(20)).unwrap();
    conn.set_timestamp(&format!("oldest_timestamp={:x}", 15))
        .unwrap();
    assert_eq!(conn.query_timestamp("get=oldest_timestamp").unwrap(), "f");
    assert!(
        get(&s, "k", Some(12)).is_err(),
        "a read below the oldest timestamp"
    );
    assert_eq!(get(&s, "k", Some(15)).unwrap().as_deref(), Some("v1"));
    assert!(
        put(&s, "j", "v0", Some(12)).is_err(),
        "a commit below the oldest timestamp"
    );
}

#[test]
fn a_timestamped_table_refuses_an_older_or_a_missing_timestamp() {
    let home = temp_home();
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    s.create("table:t", TABLE).unwrap();
    put(&s, "k", "v30", Some(30)).unwrap();
    assert!(
        put(&s, "k", "v25", Some(25)).is_err(),
        "an older timestamp on one key"
    );
    assert!(
        put(&s, "k", "plain", None).is_err(),
        "no timestamp after one"
    );
    assert_eq!(get(&s, "k", None).unwrap().as_deref(), Some("v30"));
}

#[test]
fn a_write_takes_the_commit_timestamp_set_when_it_was_made() {
    let home = temp_home();
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    s.create("table:t", TABLE).unwrap();
    // Two stamps in one transaction: each write takes the one current when
    // it is made.
    s.begin_transaction(None).unwrap();
    let cur = s.open_cursor("table:t", None).unwrap();
    s.timestamp_transaction(&format!("commit_timestamp={:x}", 50))
        .unwrap();
    cur.set_key_s("a");
    cur.set_value_u(b"a");
    cur.insert().unwrap();
    s.timestamp_transaction(&format!("commit_timestamp={:x}", 51))
        .unwrap();
    cur.set_key_s("b");
    cur.set_value_u(b"b");
    cur.insert().unwrap();
    drop(cur);
    s.commit_transaction(None).unwrap();
    assert_eq!(get(&s, "a", Some(50)).unwrap().as_deref(), Some("a"));
    assert_eq!(get(&s, "b", Some(50)).unwrap(), None);
    assert_eq!(get(&s, "b", Some(51)).unwrap().as_deref(), Some("b"));
    // A write made before any stamp takes the one given at commit.
    s.begin_transaction(None).unwrap();
    let cur = s.open_cursor("table:t", None).unwrap();
    cur.set_key_s("c");
    cur.set_value_u(b"c");
    cur.insert().unwrap();
    drop(cur);
    s.commit_transaction(Some(&format!("commit_timestamp={:x}", 60)))
        .unwrap();
    assert_eq!(get(&s, "c", Some(59)).unwrap(), None);
    assert_eq!(get(&s, "c", Some(60)).unwrap().as_deref(), Some("c"));
}

#[test]
fn a_logged_table_ignores_timestamps() {
    // Why the storage's data tables must not be logged for snapshot reads:
    // a logged table keeps no history at all.
    let home = temp_home();
    let conn = open(&home);
    let s = conn.open_session().unwrap();
    s.create("table:t", "key_format=S,value_format=u").unwrap();
    put(&s, "k", "v1", Some(10)).unwrap();
    put(&s, "k", "v2", Some(20)).unwrap();
    assert_eq!(get(&s, "k", Some(15)).unwrap().as_deref(), Some("v2"));
    assert_eq!(get(&s, "k", Some(5)).unwrap().as_deref(), Some("v2"));
}
