//! Collection stats / introspection tests (Phase 4 sub-phase 5c):
//! `get_collection_options`, `collection_is_capped`, `collection_data_size`,
//! `index_sizes`, and `scan_docs_after_id_key` in the Rust storage engine.
//! Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-stats-{}-{}", std::process::id(), n));
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

#[test]
fn get_collection_options_round_trips() {
    with_db(|st| {
        // Absent collection → empty options.
        assert!(st
            .get_collection_options("app", "missing")
            .unwrap()
            .is_empty());
        st.create_collection("app", "c").unwrap();
        st.set_collection_options(
            "app",
            "c",
            &doc! {"changeStreamPreAndPostImages": {"enabled": true}},
        )
        .unwrap();
        let opts = st.get_collection_options("app", "c").unwrap();
        assert!(opts
            .get_document("changeStreamPreAndPostImages")
            .unwrap()
            .get_bool("enabled")
            .unwrap());
        // A UUID was minted on create and is reported as a Binary.
        assert!(matches!(opts.get("uuid"), Some(Bson::Binary(_))));
    });
}

#[test]
fn synthetic_oplog_rs_options() {
    with_db(|st| {
        // local.oplog.rs reports the synthetic capped shape when oplog is on.
        let opts = st.get_collection_options("local", "oplog.rs").unwrap();
        assert!(opts.get_bool("capped").unwrap());
        assert!(opts.get_i64("max").unwrap() > 0);
        assert!(opts.get_i64("size").unwrap() > 0);
    });
}

#[test]
fn collection_is_capped_reflects_options() {
    with_db(|st| {
        st.create_collection("app", "plain").unwrap();
        assert!(!st.collection_is_capped("app", "plain").unwrap());
        st.create_collection("app", "cap").unwrap();
        st.set_collection_options("app", "cap", &doc! {"capped": true, "size": 4096})
            .unwrap();
        assert!(st.collection_is_capped("app", "cap").unwrap());
        // Absent collection is not capped.
        assert!(!st.collection_is_capped("app", "nope").unwrap());
    });
}

#[test]
fn collection_data_size_sums_blob_bytes() {
    with_db(|st| {
        assert_eq!(st.collection_data_size("app", "c").unwrap(), 0);
        let d1 = doc! {"_id": 1, "x": "hello"};
        let d2 = doc! {"_id": 2, "x": "world!!"};
        let expected = (enc(&d1).len() + enc(&d2).len()) as i64;
        st.insert_one("app", "c", &enc(&d1)).unwrap();
        st.insert_one("app", "c", &enc(&d2)).unwrap();
        assert_eq!(st.collection_data_size("app", "c").unwrap(), expected);
    });
}

#[test]
fn index_sizes_reports_id_and_secondary() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 10}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "x": 20}))
            .unwrap();
        let sizes = st.index_sizes("app", "c").unwrap();
        // _id_ is summed id_key length; x_1 is summed packed-entry length.
        assert!(sizes.get_i64("_id_").unwrap() > 0);
        assert!(sizes.get_i64("x_1").unwrap() > 0);
    });
}

#[test]
fn index_sizes_empty_collection() {
    with_db(|st| {
        // No docs → no _id_ entry, no secondary indexes.
        assert!(st.index_sizes("app", "empty").unwrap().is_empty());
    });
}

#[test]
fn scan_docs_after_id_key_incremental() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        // None → the whole collection in natural order.
        let all = st.scan_docs_after_id_key("app", "c", None).unwrap();
        assert_eq!(all.len(), 3);
        // After the first row's id_key, only rows 2 and 3 come back.
        let first_id_key = all[0].0.clone();
        let rest = st
            .scan_docs_after_id_key("app", "c", Some(&first_id_key))
            .unwrap();
        assert_eq!(rest.len(), 2);
        // After the last id_key, nothing.
        let last_id_key = all[2].0.clone();
        assert_eq!(
            st.scan_docs_after_id_key("app", "c", Some(&last_id_key))
                .unwrap()
                .len(),
            0
        );
    });
}
