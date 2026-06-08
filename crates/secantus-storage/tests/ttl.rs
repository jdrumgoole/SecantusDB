//! TTL-index pruning tests (Phase 4 sub-phase 2, slice 2e-3): `prune_ttl`
//! deletes docs whose indexed `DateTime` is older than `now - expireAfterSeconds`
//! (clock injected), leaving in-window / fieldless / non-date docs in place and
//! retracting the pruned docs' index entries. Against real WiredTiger.

use bson::{doc, Bson, DateTime, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);
const BASE_MS: i64 = 1_700_000_000_000;

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-ttl-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

/// A DateTime `secs` seconds before BASE.
fn secs_ago(secs: i64) -> DateTime {
    DateTime::from_millis(BASE_MS - secs * 1000)
}

fn now() -> DateTime {
    DateTime::from_millis(BASE_MS)
}

fn live_ids(st: &Storage) -> Vec<i32> {
    let mut v: Vec<i32> = st
        .scan_collection("app", "c")
        .unwrap()
        .iter()
        .map(|b| {
            Document::from_reader(&mut std::io::Cursor::new(b.as_slice()))
                .unwrap()
                .get_i32("_id")
                .unwrap()
        })
        .collect();
    v.sort();
    v
}

#[test]
fn prunes_only_expired_docs() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 100},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": secs_ago(200)}))
            .unwrap(); // expired
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "t": secs_ago(50)}))
            .unwrap(); // in window
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "other": 9}))
            .unwrap(); // no field
        st.insert_one("app", "c", &enc(&doc! {"_id": 4, "t": "not-a-date"}))
            .unwrap(); // non-date

        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 1);
        assert_eq!(live_ids(st), vec![2, 3, 4]);
        // 4 entries before (the non-sparse index indexes doc 3's missing `t` as
        // null, and doc 4's string value); pruning doc 1 retracts its entry -> 3.
        assert_eq!(st.index_entries("app", "c", "t_1").unwrap().len(), 3);
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(1))
            .unwrap()
            .is_none());
    });
}

#[test]
fn boundary_is_exclusive() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 100},
        )
        .unwrap();
        // age == ttl -> kept; age just over -> pruned.
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": secs_ago(100)}))
            .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "t": DateTime::from_millis(BASE_MS - 100_001)}),
        )
        .unwrap();
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 1);
        assert_eq!(live_ids(st), vec![1]);
    });
}

#[test]
fn no_ttl_index_prunes_nothing() {
    with_db(|st| {
        st.create_index("app", "c", "t_1", &doc! {"t": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": secs_ago(99999)}))
            .unwrap();
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 0);
        assert_eq!(live_ids(st), vec![1]);
    });
}
