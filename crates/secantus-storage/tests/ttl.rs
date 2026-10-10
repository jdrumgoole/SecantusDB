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

/// A partial TTL index expires only the documents its filter covers. The
/// filter used to be ignored, so the index deleted documents it did not hold
/// (mongod 8.2.11, 2026-10-10).
#[test]
fn a_partial_ttl_index_spares_documents_outside_its_filter() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 100, "partialFilterExpression": {"gone": true}},
        )
        .unwrap();
        for (id, gone) in [(1, Some(true)), (2, Some(false)), (3, None)] {
            let mut d = doc! {"_id": id, "t": secs_ago(200)};
            if let Some(g) = gone {
                d.insert("gone", g);
            }
            st.insert_one("app", "c", &enc(&d)).unwrap();
        }
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 1);
        assert_eq!(live_ids(st), vec![2, 3]);
    });
}

/// An array expires by any date in it that has passed, through a dotted path
/// too; an array nested inside the array is not looked into.
#[test]
fn an_array_of_dates_expires_by_its_earliest() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 100},
        )
        .unwrap();
        let old = secs_ago(200);
        let fresh = secs_ago(-200);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": [fresh, old]}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "t": [fresh, fresh]}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "t": [old, "x"]}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 4, "t": [[old]]}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 5, "t": ["x", 5]}))
            .unwrap();
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 2);
        assert_eq!(live_ids(st), vec![2, 4, 5]);
    });
}

/// Only a single-field index is a TTL index. A compound index carrying the
/// option (mongod refuses to create one) used to delete by its first field.
#[test]
fn a_compound_index_is_never_a_ttl_index() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1_x_1",
            &doc! {"t": 1, "x": 1},
            &doc! {"expireAfterSeconds": 100},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": secs_ago(200)}))
            .unwrap();
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 0);
        assert_eq!(live_ids(st), vec![1]);
    });
}

/// An expiry is a delete like any other: it is in the oplog, which is what a
/// change stream and crash recovery read. It used to be written nowhere.
#[test]
fn an_expiry_writes_a_delete_to_the_oplog() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 100},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 7, "t": secs_ago(200)}))
            .unwrap();
        assert_eq!(st.prune_ttl("app", "c", now()).unwrap(), 1);
        // With the async drainer (a CI lane runs every test that way) the
        // entry is written behind the delete; wait for it.
        st.flush_oplog();
        let deletes: Vec<Document> = st
            .read_oplog(0, 1000)
            .unwrap()
            .iter()
            .map(|(_, raw)| {
                Document::from_reader(&mut std::io::Cursor::new(raw.as_slice())).unwrap()
            })
            .filter(|e| e.get_str("op") == Ok("d"))
            .collect();
        assert_eq!(deletes.len(), 1, "{deletes:?}");
        assert_eq!(deletes[0].get_str("ns").unwrap(), "app.c");
        assert_eq!(
            deletes[0].get_document("o").unwrap().get("_id"),
            Some(&Bson::Int32(7))
        );
    });
}

/// The monitor counts its passes and what they deleted, runs only when its
/// period has elapsed, and stops when it is disabled.
#[test]
fn the_monitor_counts_passes_and_obeys_its_settings() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "t_1",
            &doc! {"t": 1},
            &doc! {"expireAfterSeconds": 1},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "t": secs_ago(200)}))
            .unwrap();
        let before = st.ttl_monitor();
        assert!(before.enabled);
        assert_eq!(
            (before.sleep_secs, before.passes, before.deleted),
            (0, 0, 0)
        );

        let period = std::time::Duration::from_secs(3600);
        // The first tick only starts the clock; the next is inside the period.
        assert_eq!(st.ttl_monitor_tick(period).unwrap(), None);
        assert_eq!(st.ttl_monitor_tick(period).unwrap(), None);
        // Disabled: nothing runs however short the period.
        st.set_ttl_monitor(None, Some(false));
        assert_eq!(
            st.ttl_monitor_tick(std::time::Duration::ZERO).unwrap(),
            None
        );
        st.set_ttl_monitor(None, Some(true));
        assert_eq!(
            st.ttl_monitor_tick(std::time::Duration::ZERO).unwrap(),
            Some(1)
        );
        let after = st.ttl_monitor();
        assert_eq!((after.passes, after.deleted), (1, 1));
        assert_eq!(live_ids(st), Vec::<i32>::new());

        st.set_ttl_monitor(Some(5), None);
        assert_eq!(st.ttl_monitor().sleep_secs, 5);
    });
}
