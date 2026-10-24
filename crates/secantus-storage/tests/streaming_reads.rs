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
        let first = st.records_after("db", "c", None, 2).unwrap();
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
        let rest = st.records_after("db", "c", Some(last), 100).unwrap();
        assert_eq!(
            rest.iter().map(|(_, b)| id_of(b)).collect::<Vec<_>>(),
            [2, 4, 9]
        );
        assert!(st.records_after("db", "nope", None, 10).unwrap().is_empty());
    });
}

#[test]
fn a_fetch_reads_each_document_as_it_is_now() {
    with_db(|st| {
        insert(st, &[doc! {"_id": 1, "v": 1}, doc! {"_id": 2, "v": 1}]);
        let rows = st.records_after("db", "c", None, 10).unwrap();
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
            .index_entries_from("db", "c", "x_1_y_1", b"", None, 100)
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
            .index_entries_from("db", "c", "x_1_y_1", &key, Some(rid), 100)
            .unwrap();
        assert_eq!(rest, all[3..].to_vec());
        // A seek to a key of the first field alone starts at its first entry.
        let x1 = encode_value(&Bson::Int32(1), None).unwrap();
        let from_one = st
            .index_entries_from("db", "c", "x_1_y_1", &x1, None, 100)
            .unwrap();
        assert_eq!(from_one, all[2..].to_vec());
        // An entry inserted after the position is read; a later one only.
        insert(st, &[doc! {"_id": 7, "x": 2, "y": 0}]);
        let after = st
            .index_entries_from("db", "c", "x_1_y_1", &key, Some(rid), 100)
            .unwrap();
        assert_eq!(after.len(), 4);
    });
}
