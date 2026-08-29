//! Batch insert + prune-all-collections tests (Phase 4 sub-phase 5e gap-closure):
//! `Storage::insert` (ordered / unordered, write-errors, auto-`_id`, oplog batch)
//! and `prune_ttl_all_collections`. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-batch-{}-{}", std::process::id(), n));
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

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

#[test]
fn batch_insert_all_succeed() {
    with_db(|st| {
        let docs = vec![
            enc(&doc! {"_id": 1, "x": 1}),
            enc(&doc! {"_id": 2, "x": 2}),
            enc(&doc! {"_id": 3, "x": 3}),
        ];
        let (n, errs) = st.insert("app", "c", docs, true).unwrap();
        assert_eq!(n, 3);
        assert!(errs.is_empty());
        assert_eq!(st.scan_collection("app", "c").unwrap().len(), 3);
    });
}

#[test]
fn batch_insert_assigns_missing_id() {
    with_db(|st| {
        let (n, errs) = st
            .insert(
                "app",
                "c",
                vec![enc(&doc! {"x": 1}), enc(&doc! {"x": 2})],
                true,
            )
            .unwrap();
        assert_eq!(n, 2);
        assert!(errs.is_empty());
        // Both docs got an ObjectId _id.
        for blob in st.scan_collection("app", "c").unwrap() {
            assert!(matches!(decode(&blob).get("_id"), Some(Bson::ObjectId(_))));
        }
    });
}

#[test]
fn ordered_stops_at_first_dup() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        // _id:2 collides; ordered=true stops there, so _id:3 is NOT inserted.
        let docs = vec![
            enc(&doc! {"_id": 1}),
            enc(&doc! {"_id": 2}),
            enc(&doc! {"_id": 3}),
        ];
        let (n, errs) = st.insert("app", "c", docs, true).unwrap();
        assert_eq!(n, 1);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].get_i32("index").unwrap(), 1);
        assert_eq!(errs[0].get_i32("code").unwrap(), 11000);
        // _id:1 in; _id:3 not (ordered stop).
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(1))
            .unwrap()
            .is_some());
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(3))
            .unwrap()
            .is_none());
    });
}

#[test]
fn unordered_continues_past_dups() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        let docs = vec![
            enc(&doc! {"_id": 1}),
            enc(&doc! {"_id": 2}),
            enc(&doc! {"_id": 3}),
        ];
        let (n, errs) = st.insert("app", "c", docs, false).unwrap();
        assert_eq!(n, 2); // 1 and 3
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].get_i32("index").unwrap(), 1);
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(3))
            .unwrap()
            .is_some());
    });
}

#[test]
fn unique_index_violation_is_write_error() {
    with_db(|st| {
        st.create_index("app", "c", "u_1", &doc! {"u": 1}, &doc! {"unique": true})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "u": "a"}))
            .unwrap();
        // Different _id but duplicate unique key "a".
        let (n, errs) = st
            .insert("app", "c", vec![enc(&doc! {"_id": 2, "u": "a"})], false)
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].get_i32("code").unwrap(), 11000);
        // The mongod-shaped conflict carries keyPattern / keyValue.
        assert!(errs[0]
            .get_document("keyPattern")
            .unwrap()
            .contains_key("u"));
        assert_eq!(
            errs[0]
                .get_document("keyValue")
                .unwrap()
                .get_str("u")
                .unwrap(),
            "a"
        );
    });
}

#[test]
fn batch_insert_emits_oplog_inserts() {
    with_db(|st| {
        let floor = st.oplog_tail_seq();
        st.insert(
            "app",
            "c",
            vec![enc(&doc! {"_id": 1}), enc(&doc! {"_id": 2})],
            true,
        )
        .unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(floor + 1, 100).unwrap();
        let inserts = rows
            .iter()
            .filter(|(_s, b)| decode(b).get_str("op").ok() == Some("i"))
            .count();
        assert_eq!(inserts, 2);
    });
}

#[test]
fn prune_ttl_all_collections_spans_namespaces() {
    with_db(|st| {
        use bson::DateTime;
        // TTL index (expireAfterSeconds: 0) on `t` in two collections.
        let ttl_opts = doc! {"expireAfterSeconds": 0i32};
        st.create_index("app", "a", "t_1", &doc! {"t": 1}, &ttl_opts)
            .unwrap();
        st.create_index("app", "b", "t_1", &doc! {"t": 1}, &ttl_opts)
            .unwrap();
        let old = DateTime::from_millis(0);
        st.insert_one("app", "a", &enc(&doc! {"_id": 1, "t": old}))
            .unwrap();
        st.insert_one("app", "b", &enc(&doc! {"_id": 1, "t": old}))
            .unwrap();
        st.insert_one("app", "b", &enc(&doc! {"_id": 2, "t": old}))
            .unwrap();
        // Prune as of "now" (far future) — every old doc across both colls goes.
        let now = DateTime::from_millis(10_000_000);
        let pruned = st.prune_ttl_all_collections(now).unwrap();
        assert_eq!(pruned, 3);
        assert_eq!(st.scan_collection("app", "a").unwrap().len(), 0);
        assert_eq!(st.scan_collection("app", "b").unwrap().len(), 0);
    });
}

/// One wire batch must never run as one statement transaction: a 48MB-class
/// insert's unevictable dirty content can cross WiredTiger's dirty-stall
/// fraction and livelock the engine (the Python server's mongo-rust-driver
/// `large_insert` weekly-CI wedge; here the 4G default cache masked it).
/// Against a deliberately tiny cache, the chunked insert stays bounded and
/// completes; a regression wedges (the harness timeout is the alarm).
#[test]
fn large_batch_insert_survives_a_small_cache() {
    let home = temp_home();
    let cfg = secantus_storage::wt_config("128M", 1000, false, "10MB");
    let st = Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap();
    let filler = "x".repeat(1100);
    let docs: Vec<Vec<u8>> = (0..35_000i64)
        .map(|i| enc(&doc! {"_id": i, "pad": filler.clone()}))
        .collect();
    let (inserted, errors) = st.insert("app", "c", docs, true).unwrap();
    assert_eq!(inserted, 35_000);
    assert!(errors.is_empty());
    assert!(st
        .find_by_id("app", "c", &Bson::Int64(17_321))
        .unwrap()
        .is_some());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// Ordered semantics hold across chunk boundaries: an error in a later chunk
/// stops the batch, its error `index` is in client-batch coordinates, and no
/// document after the stop lands.
#[test]
fn ordered_insert_stops_across_chunk_boundaries() {
    with_db(|st| {
        let docs: Vec<Vec<u8>> = (0..1500i64)
            .map(|i| enc(&doc! {"_id": if i == 1200 { 3 } else { i }}))
            .collect();
        let (inserted, errors) = st.insert("app", "c", docs, true).unwrap();
        assert_eq!(inserted, 1200);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].get_i32("index").unwrap(), 1200);
        assert_eq!(errors[0].get_i32("code").unwrap(), 11000);
        assert!(st
            .find_by_id("app", "c", &Bson::Int64(1499))
            .unwrap()
            .is_none());
        assert!(st
            .find_by_id("app", "c", &Bson::Int64(1199))
            .unwrap()
            .is_some());
    });
}

/// Unordered inserts keep reporting per-doc errors across chunks and insert
/// everything else.
#[test]
fn unordered_insert_reports_errors_across_chunks() {
    with_db(|st| {
        let docs: Vec<Vec<u8>> = (0..1500i64)
            .map(|i| enc(&doc! {"_id": if i == 1200 { 3 } else { i }}))
            .collect();
        let (inserted, errors) = st.insert("app", "c", docs, false).unwrap();
        assert_eq!(inserted, 1499);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].get_i32("index").unwrap(), 1200);
        assert!(st
            .find_by_id("app", "c", &Bson::Int64(1499))
            .unwrap()
            .is_some());
    });
}
