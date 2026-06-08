//! CRUD-core integration tests for `secantus-storage` against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-storage-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `body` against a fresh on-disk database, then **close it before** removing
/// the directory — deleting WiredTiger's files out from under an open connection
/// makes its close-time checkpoint fail (stat of a now-unlinked file).
fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st); // close (final checkpoint) before the files disappear
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn dec(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

#[test]
fn insert_find_roundtrip_various_id_types() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "one"}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": "x", "v": "ex"}))
            .unwrap();
        let oid = bson::oid::ObjectId::new();
        st.insert_one("app", "c", &enc(&doc! {"_id": oid, "v": "obj"}))
            .unwrap();

        assert_eq!(
            dec(&st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap())
                .get_str("v")
                .unwrap(),
            "one"
        );
        assert_eq!(
            dec(&st
                .find_by_id("app", "c", &Bson::String("x".into()))
                .unwrap()
                .unwrap())
            .get_str("v")
            .unwrap(),
            "ex"
        );
        assert_eq!(
            dec(&st
                .find_by_id("app", "c", &Bson::ObjectId(oid))
                .unwrap()
                .unwrap())
            .get_str("v")
            .unwrap(),
            "obj"
        );
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(99))
            .unwrap()
            .is_none());
    });
}

#[test]
fn auto_assigns_objectid_when_id_absent() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"v": 1})).unwrap();
        let docs = st.scan_collection("app", "c").unwrap();
        assert_eq!(docs.len(), 1);
        assert!(matches!(dec(&docs[0]).get("_id"), Some(Bson::ObjectId(_))));
    });
}

#[test]
fn duplicate_id_is_rejected() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();
        let err = st
            .insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "b"}))
            .unwrap_err();
        assert!(matches!(err, StorageError::DuplicateId), "got {err}");
        // The original is untouched.
        assert_eq!(
            dec(&st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap())
                .get_str("v")
                .unwrap(),
            "a"
        );
    });
}

#[test]
fn scan_is_in_cross_type_natural_order() {
    with_db(|st| {
        // BSON canonical order: numbers < strings < ObjectId; numbers sort by
        // value across int/double.
        let oid = bson::oid::ObjectId::new();
        for d in [
            doc! {"_id": "apple"},
            doc! {"_id": oid},
            doc! {"_id": 10_i64},
            doc! {"_id": 1.5_f64},
            doc! {"_id": 2_i32},
        ] {
            st.insert_one("app", "c", &enc(&d)).unwrap();
        }
        let ids: Vec<Bson> = st
            .scan_collection("app", "c")
            .unwrap()
            .iter()
            .map(|b| dec(b).get("_id").unwrap().clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                Bson::Double(1.5),
                Bson::Int32(2),
                Bson::Int64(10),
                Bson::String("apple".into()),
                Bson::ObjectId(oid),
            ],
            "numeric cross-type order, then strings, then ObjectId"
        );
    });
}

#[test]
fn replace_and_delete() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();

        // Replace preserves _id even if the new body omits / changes it.
        assert!(st
            .replace_by_id(
                "app",
                "c",
                &Bson::Int32(1),
                &enc(&doc! {"v": "b", "_id": 999})
            )
            .unwrap());
        let d = dec(&st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap());
        assert_eq!(d.get_str("v").unwrap(), "b");
        assert_eq!(d.get("_id"), Some(&Bson::Int32(1)));

        // Replace of a missing _id -> false.
        assert!(!st
            .replace_by_id("app", "c", &Bson::Int32(7), &enc(&doc! {"v": "z"}))
            .unwrap());

        assert!(st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap());
        assert!(!st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap());
        assert!(st.scan_collection("app", "c").unwrap().is_empty());
    });
}

#[test]
fn collections_and_dbs_are_isolated() {
    with_db(|st| {
        st.insert_one("db1", "a", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("db1", "b", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("db2", "a", &enc(&doc! {"_id": 1})).unwrap();

        assert!(st.collection_exists("db1", "a").unwrap());
        assert!(!st.collection_exists("db1", "zzz").unwrap());

        let mut c1 = st.list_collections("db1").unwrap();
        c1.sort();
        assert_eq!(c1, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(st.list_collections("db2").unwrap(), vec!["a".to_string()]);

        // Same _id in different (db, coll) doesn't collide.
        assert_eq!(st.scan_collection("db1", "a").unwrap().len(), 1);
        assert_eq!(st.scan_collection("db2", "a").unwrap().len(), 1);
    });
}

#[test]
fn data_survives_reopen() {
    let home = temp_home();
    let hp = home.to_str().unwrap().to_string();
    {
        let st = Storage::open(&hp).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 42, "v": "persisted"}))
            .unwrap();
        drop(st);
    }
    {
        let st = Storage::open(&hp).unwrap();
        let d = dec(&st
            .find_by_id("app", "c", &Bson::Int32(42))
            .unwrap()
            .unwrap());
        assert_eq!(d.get_str("v").unwrap(), "persisted");
        assert!(st.collection_exists("app", "c").unwrap());
        drop(st);
    }
    let _ = std::fs::remove_dir_all(&home);
}
