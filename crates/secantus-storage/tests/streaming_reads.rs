//! The reads the MongoDB server's streaming cursors resume: a collection
//! scan after a RecordId, documents fetched by RecordId as they are now, and
//! an index walk after an entry -- each seeing writes made since the last
//! call, as mongod's restored executor does.

use bson::{doc, Bson, Document};
use secantus_core::sortkey::{encode_value, split_compound};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home: PathBuf =
        std::env::temp_dir().join(format!("secantus-stream-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn id_of(blob: &[u8]) -> i32 {
    Document::from_reader(&mut std::io::Cursor::new(blob))
        .unwrap()
        .get_i32("_id")
        .unwrap()
}

fn insert(st: &Storage, docs: &[Document]) {
    let blobs = docs.iter().map(|d| bson::to_vec(d).unwrap()).collect();
    st.insert("db", "c", blobs, true).unwrap();
}

#[test]
fn a_collection_scan_resumes_after_its_record_id() {
    with_db(|st| {
        insert(st, &(0..5).map(|i| doc! {"_id": i}).collect::<Vec<_>>());
        let first = st.records_after("db", "c", None, 2, false).unwrap();
        assert_eq!(
            first.iter().map(|(_, b)| id_of(b)).collect::<Vec<_>>(),
            [0, 1]
        );
        let last = first[1].0;
        // A document deleted ahead of the position is not read; one inserted
        // after it is.
        st.delete_matching("db", "c", &doc! {"_id": 3}, 1, &Document::new(), None)
            .unwrap();
        insert(st, &[doc! {"_id": 9}]);
        let rest = st.records_after("db", "c", Some(last), 100, false).unwrap();
        assert_eq!(
            rest.iter().map(|(_, b)| id_of(b)).collect::<Vec<_>>(),
            [2, 4, 9]
        );
        assert!(st
            .records_after("db", "nope", None, 10, false)
            .unwrap()
            .is_empty());
    });
}

#[test]
fn a_fetch_reads_each_document_as_it_is_now() {
    with_db(|st| {
        insert(st, &[doc! {"_id": 1, "v": 1}, doc! {"_id": 2, "v": 1}]);
        let rows = st.records_after("db", "c", None, 10, false).unwrap();
        let rids: Vec<i64> = rows.iter().map(|(r, _)| *r).collect();
        st.delete_matching("db", "c", &doc! {"_id": 2}, 1, &Document::new(), None)
            .unwrap();
        let got = st.records_by_id("db", "c", &rids).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(id_of(got[0].as_deref().unwrap()), 1);
        assert!(got[1].is_none());
    });
}

#[test]
fn an_index_walk_resumes_after_its_entry() {
    with_db(|st| {
        insert(
            st,
            &(0..6)
                .map(|i| doc! {"_id": i, "x": i % 3, "y": -99 - i})
                .collect::<Vec<_>>(),
        );
        st.create_index("db", "c", "x_1_y_1", &doc! {"x": 1, "y": 1}, &doc! {})
            .unwrap();
        let all = st
            .index_entries_from("db", "c", "x_1_y_1", b"", None, 100, false)
            .unwrap();
        assert_eq!(all.len(), 6);
        // In key order, and every key splits into its two fields' encodings.
        let x0 = encode_value(&Bson::Int32(0), None).unwrap();
        for (key, _) in &all {
            let parts = split_compound(key, 2).expect("a compound key");
            assert!(parts[0].len() > 1 && parts[1].len() > 1);
        }
        assert!(all.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(all[0].0.starts_with(&x0));
        // From the third entry on, exclusive of it.
        let (key, rid) = all[2].clone();
        let rest = st
            .index_entries_from("db", "c", "x_1_y_1", &key, Some(rid), 100, false)
            .unwrap();
        assert_eq!(rest, all[3..].to_vec());
        // A seek to a key of the first field alone starts at its first entry.
        let x1 = encode_value(&Bson::Int32(1), None).unwrap();
        let from_one = st
            .index_entries_from("db", "c", "x_1_y_1", &x1, None, 100, false)
            .unwrap();
        assert_eq!(from_one, all[2..].to_vec());
        // An entry inserted after the position is read; a later one only.
        insert(st, &[doc! {"_id": 7, "x": 2, "y": 0}]);
        let after = st
            .index_entries_from("db", "c", "x_1_y_1", &key, Some(rid), 100, false)
            .unwrap();
        assert_eq!(after.len(), 4);
    });
}

#[test]
fn a_backward_collection_scan_resumes_before_its_record_id() {
    with_db(|st| {
        insert(st, &(0..5).map(|i| doc! {"_id": i}).collect::<Vec<_>>());
        let first = st.records_after("db", "c", None, 2, true).unwrap();
        assert_eq!(
            first.iter().map(|(_, b)| id_of(b)).collect::<Vec<_>>(),
            [4, 3]
        );
        // A document deleted ahead of the position is not read; one inserted
        // since lands behind it, at the end of the collection.
        st.delete_matching("db", "c", &doc! {"_id": 1}, 1, &Document::new(), None)
            .unwrap();
        insert(st, &[doc! {"_id": 9}]);
        let rest = st
            .records_after("db", "c", Some(first[1].0), 100, true)
            .unwrap();
        assert_eq!(
            rest.iter().map(|(_, b)| id_of(b)).collect::<Vec<_>>(),
            [2, 0]
        );
        assert!(st
            .records_after("db", "nope", None, 10, true)
            .unwrap()
            .is_empty());
    });
}

#[test]
fn a_backward_index_walk_resumes_before_its_entry() {
    with_db(|st| {
        insert(
            st,
            &(0..6)
                .map(|i| doc! {"_id": i, "x": i % 3, "y": -99 - i})
                .collect::<Vec<_>>(),
        );
        st.create_index("db", "c", "x_1_y_1", &doc! {"x": 1, "y": 1}, &doc! {})
            .unwrap();
        let all = st
            .index_entries_from("db", "c", "x_1_y_1", b"", None, 100, false)
            .unwrap();
        let mut reversed = all.clone();
        reversed.reverse();
        // From the highest key down: every entry, last first.
        let top = encode_value(&Bson::MaxKey, None).unwrap();
        let down = st
            .index_entries_from("db", "c", "x_1_y_1", &top, None, 100, true)
            .unwrap();
        assert_eq!(down, reversed);
        // Before the fourth entry, exclusive of it.
        let (key, rid) = all[3].clone();
        let rest = st
            .index_entries_from("db", "c", "x_1_y_1", &key, Some(rid), 100, true)
            .unwrap();
        assert_eq!(rest, reversed[3..].to_vec());
        // A key of the first field alone is below every entry that extends
        // it, so a walk down from it reads only the `x: 0` entries. (A
        // streaming cursor's start names every field.)
        let x1 = encode_value(&Bson::Int32(1), None).unwrap();
        let from_one = st
            .index_entries_from("db", "c", "x_1_y_1", &x1, None, 100, true)
            .unwrap();
        assert_eq!(from_one, reversed[4..].to_vec());
        // With every field named, the walk starts at that entry.
        let (k2, r2) = all[3].clone();
        let from_entry = st
            .index_entries_from("db", "c", "x_1_y_1", &k2, None, 100, true)
            .unwrap();
        assert_eq!(from_entry.first(), Some(&(k2, r2)));
    });
}

#[test]
fn the_id_index_walks_the_documents_id_keys() {
    with_db(|st| {
        insert(
            st,
            &[5, 1, 4, 2, 3]
                .iter()
                .map(|i| doc! {"_id": *i})
                .collect::<Vec<_>>(),
        );
        let key = |i: i32| encode_value(&Bson::Int32(i), None).unwrap();
        let ids = |rows: &[(Vec<u8>, i64)]| -> Vec<i32> {
            let rids: Vec<i64> = rows.iter().map(|(_, r)| *r).collect();
            st.records_by_id("db", "c", &rids)
                .unwrap()
                .iter()
                .map(|d| id_of(d.as_deref().unwrap()))
                .collect()
        };
        let all = st
            .index_entries_from("db", "c", "_id_", b"", None, 100, false)
            .unwrap();
        assert_eq!(ids(&all), [1, 2, 3, 4, 5]);
        assert_eq!(all[0].0, key(1));
        let from_three = st
            .index_entries_from("db", "c", "_id_", &key(3), None, 100, false)
            .unwrap();
        assert_eq!(ids(&from_three), [3, 4, 5]);
        let past_three = st
            .index_entries_from("db", "c", "_id_", &key(3), Some(0), 100, false)
            .unwrap();
        assert_eq!(ids(&past_three), [4, 5]);
        let down_from_three = st
            .index_entries_from("db", "c", "_id_", &key(3), None, 100, true)
            .unwrap();
        assert_eq!(ids(&down_from_three), [3, 2, 1]);
        let below_three = st
            .index_entries_from("db", "c", "_id_", &key(3), Some(0), 2, true)
            .unwrap();
        assert_eq!(ids(&below_three), [2, 1]);
        assert!(st
            .index_entries_from("db", "nope", "_id_", b"", None, 10, false)
            .unwrap()
            .is_empty());
    });
}
