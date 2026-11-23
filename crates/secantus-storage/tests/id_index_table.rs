//! The `_id_` index is a WiredTiger table of its own, `index-<UUID>`, in
//! mongod 8.2.11's format (storage-model plan, phase 4d): each entry's key is
//! the KeyString of `_id` alone, its value the RecordId (as
//! `append_record_id_long` writes it) and then the TypeBits. It replaces the
//! `_id -> RecordId` table every collection used to share
//! (`secantus_natural_seq`).
//!
//! The first tests rebuild every row of the `_id_` tables a real mongod 8.2.11
//! wrote (`tests/fixtures/storage_model/mongod-8.2.11-layout.json`, phase 0b)
//! by inserting the same documents, and compare key and value byte for byte.

use bson::{doc, Bson, Document};
use secantus_storage::{IndexPos, Storage};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-idtab-{}-{}", std::process::id(), n));
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

fn insert(st: &Storage, coll: &str, docs: &[Document]) {
    let (n, errors) = st
        .insert("app", coll, docs.iter().map(enc).collect(), true)
        .unwrap();
    assert_eq!((n, errors.len()), (docs.len(), 0), "{errors:?}");
}

fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/storage_model/mongod-8.2.11-layout.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

/// The fixture's `index_tables[name]` as hex key -> hex value.
fn mongod_rows(name: &str) -> BTreeMap<String, String> {
    fixture()["index_tables"][name]
        .as_array()
        .unwrap_or_else(|| panic!("no index table {name}"))
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_string(),
                r["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This store's `_id_` table, as hex key -> hex value.
fn our_rows(st: &Storage, coll: &str) -> BTreeMap<String, String> {
    st.index_table_rows("app", coll, "_id_")
        .unwrap()
        .into_iter()
        .map(|(k, v)| (hex(&k), hex(&v)))
        .collect()
}

#[test]
fn the_id_table_of_app_c_matches_mongod_byte_for_byte() {
    with_db(|st| {
        // The six documents behind `app.c` and `{_id: 99, a: 9}`, RecordIds
        // 1 to 7 in insert order (`tools/probes/mongod_store_layout.py`).
        let mut docs: Vec<Document> = (0..6)
            .map(|i| doc! {"_id": i, "a": i % 3, "b": format!("s{i}"), "t": [i, i + 1]})
            .collect();
        docs.push(doc! {"_id": 99, "a": 9});
        insert(st, "c", &docs);
        assert_eq!(our_rows(st, "c"), mongod_rows("app.c/_id_"));
    });
}

#[test]
fn the_id_table_of_app_ks_matches_mongod_byte_for_byte() {
    with_db(|st| {
        // `app.ks` holds one document per KeyString test value, its `_id`
        // the position in the list, and `{_id: 1000}` last.
        let n = mongod_rows("app.ks/_id_").len() as i32 - 1;
        let mut docs: Vec<Document> = (0..n).map(|i| doc! {"_id": i}).collect();
        docs.push(doc! {"_id": 1000});
        insert(st, "ks", &docs);
        let got = our_rows(st, "ks");
        let want = mongod_rows("app.ks/_id_");
        assert_eq!(got.len(), want.len());
        assert_eq!(got, want);
    });
}

#[test]
fn string_ids_match_mongods_views_catalog_byte_for_byte() {
    with_db(|st| {
        // `app.system.views`' `_id` is the view's namespace: a string key,
        // whose TypeBits are empty.
        insert(
            st,
            "system.views",
            &[doc! {"_id": "app.ts"}, doc! {"_id": "app.v_view"}],
        );
        assert_eq!(
            our_rows(st, "system.views"),
            mongod_rows("app.system.views/_id_")
        );
    });
}

#[test]
fn an_id_of_another_type_keeps_its_type_bits_in_the_value() {
    with_db(|st| {
        // A double `_id` is keyed as the number it is, and its TypeBits say
        // it was a double: they follow the RecordId in the value.
        insert(st, "c", &[doc! {"_id": 0.5}, doc! {"_id": 2i64}]);
        let rows = st.index_table_rows("app", "c", "_id_").unwrap();
        assert_eq!(rows.len(), 2);
        for (key, value) in &rows {
            // The RecordId is the first two bytes (1 and 2), the TypeBits
            // the rest; the key ends in the KeyString end byte.
            assert_eq!(key.last(), Some(&0x04));
            assert!(value.len() > 2, "{value:?}");
        }
    });
}

#[test]
fn the_id_index_is_listed_in_the_catalog_with_its_own_ident() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1}]);
        let table = st.index_table_name("app", "c", "_id_").unwrap().unwrap();
        let coll_table = st.collection_table_name("app", "c").unwrap().unwrap();
        assert!(table.starts_with("table:index-"), "{table}");
        assert_ne!(table, coll_table);
        let entry = st.mdb_catalog_entry("app", "c").unwrap().unwrap();
        let idents = entry.get_document("idxIdent").unwrap();
        assert_eq!(
            idents.get_str("_id_").unwrap(),
            table.trim_start_matches("table:")
        );
        assert!(st.wt_table_names().unwrap().contains(&table));
    });
}

#[test]
fn the_store_holds_no_shared_id_or_claim_table() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "u": 1}]);
        st.create_index("app", "c", "u_1", &doc! {"u": 1}, &doc! {"unique": true})
            .unwrap();
        insert(st, "c", &[doc! {"_id": 2, "u": 2}]);
        let names = st.wt_table_names().unwrap();
        for gone in ["table:secantus_natural_seq", "table:secantus_unique_keys"] {
            assert!(!names.iter().any(|n| n == gone), "{gone} in {names:?}");
        }
    });
}

#[test]
fn a_duplicate_id_is_refused_and_the_first_stays() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "v": "first"}]);
        let (n, errors) = st
            .insert("app", "c", vec![enc(&doc! {"_id": 1, "v": "second"})], true)
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].get_i32("code"), Ok(11000));
        assert!(errors[0].get_str("errmsg").unwrap().starts_with(
            "E11000 duplicate key error collection: app.c index: _id_ dup key: { _id: 1 }"
        ));
        let kept = st.find_by_id("app", "c", &Bson::Int32(1)).unwrap().unwrap();
        assert_eq!(
            bson::from_slice::<Document>(&kept).unwrap().get_str("v"),
            Ok("first")
        );
        // One value written as another number type is the same `_id`.
        let (n, errors) = st
            .insert("app", "c", vec![enc(&doc! {"_id": 1.0})], true)
            .unwrap();
        assert_eq!((n, errors.len()), (0, 1));
    });
}

#[test]
fn a_collection_with_a_collation_takes_it_in_its_id_index() {
    with_db(|st| {
        st.create_collection_with_options(
            "app",
            "ci",
            &doc! {"collation": {"locale": "en", "strength": 2}},
        )
        .unwrap();
        insert(st, "ci", &[doc! {"_id": "a"}]);
        // `"A"` is the same `_id` under strength 2: the key is the collation's.
        let (n, errors) = st
            .insert("app", "ci", vec![enc(&doc! {"_id": "A"})], true)
            .unwrap();
        assert_eq!((n, errors.len()), (0, 1));
        assert_eq!(errors[0].get_i32("code"), Ok(11000));
        // A lookup of either spelling finds it.
        assert!(st
            .find_by_id("app", "ci", &Bson::String("A".into()))
            .unwrap()
            .is_some());
        // Delete frees the value.
        assert!(st
            .delete_by_id("app", "ci", &Bson::String("A".into()))
            .unwrap());
        insert(st, "ci", &[doc! {"_id": "A"}]);
        assert_eq!(st.index_table_rows("app", "ci", "_id_").unwrap().len(), 1);
    });
}

#[test]
fn deleting_a_document_removes_its_id_entry_and_updating_leaves_it() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "v": 1}, doc! {"_id": 2, "v": 2}]);
        assert_eq!(st.index_table_rows("app", "c", "_id_").unwrap().len(), 2);
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"v": 9}},
            false,
            false,
            &[],
            &secantus_core::vars::Vars::new(),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(st.index_table_rows("app", "c", "_id_").unwrap().len(), 2);
        assert!(st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap());
        let rows = st.index_table_rows("app", "c", "_id_").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(1))
            .unwrap()
            .is_none());
        assert!(st
            .find_by_id("app", "c", &Bson::Int32(2))
            .unwrap()
            .is_some());
    });
}

#[test]
fn a_rename_moves_the_id_table_whole_and_a_drop_drops_it() {
    with_db(|st| {
        insert(st, "old", &[doc! {"_id": 1}, doc! {"_id": 2}]);
        let before = st.index_table_name("app", "old", "_id_").unwrap().unwrap();
        let rows = our_rows(st, "old");
        assert_eq!(
            st.rename_collection("app", "old", "app", "new", false)
                .unwrap(),
            (true, None)
        );
        // The table is the same one, under the new name, rows untouched.
        assert_eq!(
            st.index_table_name("app", "new", "_id_").unwrap(),
            Some(before.clone())
        );
        assert_eq!(our_rows(st, "new"), rows);
        assert!(st
            .find_by_id("app", "new", &Bson::Int32(2))
            .unwrap()
            .is_some());
        assert!(st.drop_collection("app", "new").unwrap());
        for _ in 0..200 {
            st.reap_dropped_tables();
            if !st.wt_table_names().unwrap().contains(&before) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!st.wt_table_names().unwrap().contains(&before));
        // The name is free again, with no entry left to refuse an `_id`.
        insert(st, "new", &[doc! {"_id": 1}]);
    });
}

#[test]
fn a_time_series_collection_and_the_profile_have_no_id_index() {
    with_db(|st| {
        st.create_collection_with_options("app", "ts", &doc! {"timeseries": {"timeField": "t"}})
            .unwrap();
        // A time-series collection is mongod's view over its buckets (a
        // `system.views` document, storage-model phase 7): no table of its
        // own, so no `_id_` table either.
        assert_eq!(st.collection_table_name("app", "ts").unwrap(), None);
        assert_eq!(st.index_table_name("app", "ts", "_id_").unwrap(), None);
        insert(st, "system.profile", &[doc! {"op": "query"}]);
        assert_eq!(
            st.index_table_name("app", "system.profile", "_id_")
                .unwrap(),
            None
        );
    });
}

#[test]
fn the_id_index_reads_in_id_order_by_value() {
    with_db(|st| {
        insert(
            st,
            "c",
            &[doc! {"_id": 5}, doc! {"_id": 1.5}, doc! {"_id": 3i64}],
        );
        let rows = st
            .index_entries_from("app", "c", "_id_", IndexPos::Edge, None, 10, false)
            .unwrap();
        let ids: Vec<Document> = st
            .records_by_id(
                "app",
                "c",
                &rows.iter().map(|r| r.rid.clone()).collect::<Vec<_>>(),
            )
            .unwrap()
            .into_iter()
            .map(|d| bson::from_slice(&d.unwrap()).unwrap())
            .collect();
        let ids: Vec<f64> = ids
            .iter()
            .map(|d| match d.get("_id").unwrap() {
                Bson::Int32(n) => f64::from(*n),
                Bson::Int64(n) => *n as f64,
                Bson::Double(n) => *n,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, [1.5, 3.0, 5.0]);
    });
}

#[test]
fn an_id_range_reads_between_its_bounds_in_key_order() {
    with_db(|st| {
        let docs: Vec<Document> = (0..20).map(|i| doc! {"_id": i}).collect();
        insert(st, "c", &docs);
        let got: Vec<i32> = st
            .find_matching("app", "c", &doc! {"_id": {"$gte": 5, "$lt": 9}})
            .unwrap()
            .iter()
            .map(|b| {
                bson::from_slice::<Document>(b)
                    .unwrap()
                    .get_i32("_id")
                    .unwrap()
            })
            .collect();
        assert_eq!(got, [5, 6, 7, 8]);
    });
}
