//! The index keys a view's or a time-series collection's definition counts
//! (RM-363): mongod keeps each as a document of its database's
//! `system.views`, so creating one inserts that document's `_id` key,
//! dropping it deletes the key, and a `collMod` of a view updates it in
//! place. Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-views-{}-{}", std::process::id(), n));
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

/// The keys `op` inserted and deleted, as an operation counts them.
fn keys(op: impl FnOnce()) -> (Option<i64>, Option<i64>) {
    let scope = secantus_storage::operation_scope();
    op();
    let c = scope.counts();
    (c.keys_inserted, c.keys_deleted)
}

fn view() -> Document {
    doc! { "viewOn": "c", "viewPipeline": [] }
}

#[test]
fn a_views_definition_counts_its_key() {
    with_db(|st| {
        st.create_collection("app", "c").unwrap();
        let created = keys(|| {
            assert!(st
                .create_collection_with_options("app", "v", &view())
                .unwrap())
        });
        assert_eq!(created, (Some(1), None));
        // A create that finds it there inserts nothing.
        let again = keys(|| {
            assert!(!st
                .create_collection_with_options("app", "v", &view())
                .unwrap())
        });
        assert_eq!(again, (None, None));
        let modified = keys(|| {
            st.coll_mod("app", "v", &doc! { "viewPipeline": [{"$match": {}}] })
                .unwrap()
        });
        assert_eq!(modified, (Some(0), Some(0)));
        let dropped = keys(|| assert!(st.drop_collection("app", "v").unwrap()));
        assert_eq!(dropped, (None, Some(1)));
    });
}

#[test]
fn a_time_series_definition_counts_its_key() {
    with_db(|st| {
        let options = doc! { "timeseries": { "timeField": "t" } };
        let created = keys(|| {
            assert!(st
                .create_collection_with_options("app", "ts", &options)
                .unwrap())
        });
        assert_eq!(created, (Some(1), None));
        let dropped = keys(|| assert!(st.drop_collection("app", "ts").unwrap()));
        assert_eq!(dropped, (None, Some(1)));
    });
}

#[test]
fn a_collection_counts_no_definition_key() {
    with_db(|st| {
        let created = keys(|| assert!(st.create_collection("app", "c").unwrap()));
        assert_eq!(created, (None, None));
        let modified = keys(|| st.coll_mod("app", "c", &doc! { "validator": {} }).unwrap());
        assert_eq!(modified, (None, None));
        let dropped = keys(|| assert!(st.drop_collection("app", "c").unwrap()));
        assert_eq!(dropped, (None, None));
    });
}
