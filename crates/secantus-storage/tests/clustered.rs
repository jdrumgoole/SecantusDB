//! A clustered collection as mongod 8.2.11 keeps it (storage-model plan,
//! phase 5): its table is keyed by the KeyString of `_id` (`key_format=u`, no
//! TypeBits and no end byte), its value is the document alone, and it has no
//! `_id_` table. A time-series collection's buckets and
//! `config.system.preimages` are clustered the same way; the buckets' table
//! is zstd-compressed, as mongod makes it.
//!
//! The keys are compared with what a real mongod wrote
//! (`tests/fixtures/storage_model/mongod-8.2.11-layout.json`).

use bson::{doc, Bson, Document};
use secantus_storage::{IndexPos, RecordId, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home: PathBuf =
        std::env::temp_dir().join(format!("secantus-clustered-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn clustered(st: &Storage, coll: &str) {
    st.create_collection_with_options(
        "app",
        coll,
        &doc! {"clusteredIndex": {"v": 2, "key": {"_id": 1}, "name": "_id_", "unique": true}},
    )
    .unwrap();
}

fn insert(st: &Storage, coll: &str, docs: &[Document]) {
    let (n, errors) = st
        .insert("app", coll, docs.iter().map(enc).collect(), true)
        .unwrap();
    assert_eq!((n, errors.len()), (docs.len(), 0), "{errors:?}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn keys(st: &Storage, coll: &str) -> Vec<String> {
    st.collection_table_rows("app", coll)
        .unwrap()
        .into_iter()
        .map(|(rid, _)| hex(rid.as_bytes().expect("a clustered table is keyed by bytes")))
        .collect()
}

#[test]
fn the_key_is_the_keystring_of_id_and_the_value_the_document_alone() {
    with_db(|st| {
        clustered(st, "clustered");
        // The fixture's `app.clustered`: `_id`s 3 (int), "s" and an ObjectId
        // inserted by mongod 8.2.11, in this order, read back in key order.
        let oid = bson::oid::ObjectId::parse_str("000000000000000000000002").unwrap();
        let docs = [
            doc! {"_id": "s", "x": 2},
            doc! {"_id": 1, "x": 1},
            doc! {"_id": oid, "x": 3},
        ];
        insert(st, "clustered", &docs);
        // mongod's rows: `2b02` (1), `3c7300` ("s"), `64` + the ObjectId.
        assert_eq!(
            keys(st, "clustered"),
            ["2b02", "3c7300", "64000000000000000000000002"]
        );
        let rows = st.collection_table_rows("app", "clustered").unwrap();
        assert_eq!(rows[0].1, enc(&docs[1]), "the value is the document alone");
        // No `_id_` table, as mongod has none.
        assert_eq!(
            st.index_table_name("app", "clustered", "_id_").unwrap(),
            None
        );
        let table = st
            .collection_table_name("app", "clustered")
            .unwrap()
            .unwrap();
        let cfg = st.table_file_config(&table).unwrap().unwrap();
        assert!(cfg.contains("key_format=u"), "{cfg}");
        assert!(cfg.contains("block_compressor=snappy"), "{cfg}");
        // `_mdb_catalog` lists no `_id_` index and no `idxIdent` for it.
        let entry = st.mdb_catalog_entry("app", "clustered").unwrap().unwrap();
        assert!(entry.get_document("idxIdent").unwrap().is_empty());
        assert!(entry
            .get_document("md")
            .unwrap()
            .get_array("indexes")
            .unwrap()
            .is_empty());
    });
}

#[test]
fn the_buckets_table_is_zstd() {
    with_db(|st| {
        st.create_collection_with_options(
            "app",
            "system.buckets.ts",
            &doc! {"clusteredIndex": true},
        )
        .unwrap();
        let table = st
            .collection_table_name("app", "system.buckets.ts")
            .unwrap()
            .unwrap();
        let cfg = st.table_file_config(&table).unwrap().unwrap();
        assert!(cfg.contains("key_format=u"), "{cfg}");
        assert!(cfg.contains("block_compressor=zstd"), "{cfg}");
    });
}

#[test]
fn a_duplicate_id_is_refused_and_a_point_read_finds_the_row() {
    with_db(|st| {
        clustered(st, "c");
        insert(st, "c", &[doc! {"_id": 5, "v": 1}]);
        let (n, errors) = st
            .insert("app", "c", vec![enc(&doc! {"_id": 5, "v": 2})], true)
            .unwrap();
        assert_eq!((n, errors.len()), (0, 1));
        assert_eq!(errors[0].get_i32("code"), Ok(11000));
        let found = st
            .find_by_id("app", "c", &Bson::Int32(5))
            .unwrap()
            .expect("the row");
        assert_eq!(
            bson::from_slice::<Document>(&found).unwrap().get_i32("v"),
            Ok(1)
        );
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(6))
            .unwrap()
            .is_none());
    });
}

#[test]
fn update_delete_and_a_secondary_index_work_on_the_keys() {
    with_db(|st| {
        clustered(st, "c");
        insert(
            st,
            "c",
            &(0..20)
                .map(|i| doc! {"_id": i, "x": i % 4})
                .collect::<Vec<_>>(),
        );
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        // The fixture's `app.clustered/x_1` entry for `x: 1`, `_id: 1`:
        // the key, the end byte, then the RecordId's bytes and their size.
        let rows = st.index_table_rows("app", "c", "x_1").unwrap();
        assert_eq!(rows.len(), 20);
        assert!(rows
            .iter()
            .any(|(k, _)| hex(k) == "2b0204".to_string() + "2b02" + "02"));
        // An update moves an entry and keeps the row.
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"x": 9}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
        let hit = st
            .index_entries_from("app", "c", "x_1", IndexPos::Edge, None, 100, false)
            .unwrap();
        assert_eq!(hit.len(), 20);
        let by_x9: Vec<&secantus_storage::IndexRow> = hit
            .iter()
            .filter(|r| matches!(&r.rid, RecordId::Bytes(b) if b == &[0x2b, 0x02]))
            .collect();
        assert_eq!(by_x9.len(), 1);
        // A delete removes the row and its entries.
        assert_eq!(
            st.delete_matching("app", "c", &doc! {"_id": 1}, 1, &Document::new(), None)
                .unwrap(),
            1
        );
        assert_eq!(st.index_table_rows("app", "c", "x_1").unwrap().len(), 19);
        assert_eq!(st.collection_table_rows("app", "c").unwrap().len(), 19);
    });
}
