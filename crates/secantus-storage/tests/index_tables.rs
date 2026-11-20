//! Every secondary index is a WiredTiger table of its own, `index-<UUID>`,
//! in mongod 8.2.11's format (storage-model plan, phase 4a and 4b): each
//! entry's key is the KeyString of the index key's values followed by the
//! RecordId, its value the TypeBits; the table is named in the collection's
//! `_mdb_catalog` row (`idxIdent`) and goes when the index, the collection
//! or the transaction that made it does.

use bson::{doc, Bson, Document};
use secantus_core::collation;
use secantus_core::keystring::{Builder, Ordering, Version};
use secantus_storage::{IndexPos, Storage};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-ixtab-{}-{}", std::process::id(), n));
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
    st.insert("app", coll, docs.iter().map(enc).collect(), true)
        .unwrap();
}

/// Each stored document with its RecordId.
fn documents(st: &Storage, coll: &str) -> Vec<(i64, Document)> {
    st.records_after("app", coll, None, 1000, false)
        .unwrap()
        .into_iter()
        .map(|(rid, blob)| (rid, bson::from_slice(&blob).unwrap()))
        .collect()
}

/// A collation's sort-key function, as KeyString takes it.
type Transform<'a> = &'a dyn Fn(&str) -> Vec<u8>;

/// What mongod's KeyString writes for one index entry: the values in the
/// directions of the key pattern, then the RecordId, and the TypeBits.
fn expected_entry(
    dirs: &[i32],
    values: &[Bson],
    rid: i64,
    transform: Option<Transform<'_>>,
) -> (Vec<u8>, Vec<u8>) {
    let mut b = Builder::new(Version::V1, Ordering::from_directions(dirs));
    for v in values {
        b.append_bson_element_with(v, transform).unwrap();
    }
    b.append_record_id_long(rid).unwrap();
    let (key, tb) = b.finish().unwrap();
    (key, tb.serialize())
}

fn rows(st: &Storage, coll: &str, index: &str) -> BTreeSet<(Vec<u8>, Vec<u8>)> {
    st.index_table_rows("app", coll, index)
        .unwrap()
        .into_iter()
        .collect()
}

/// The tables whose name starts with `prefix`, once the drop-pending reaper
/// has dropped what it can: a table another session still has a cursor on is
/// kept for its next pass, so the reaper is run until the answer stops
/// changing.
fn tables_named(st: &Storage, prefix: &str) -> Vec<String> {
    let mut last: Option<Vec<String>> = None;
    let mut stable = 0;
    for _ in 0..200 {
        st.reap_dropped_tables();
        let now: Vec<String> = st
            .wt_table_names()
            .unwrap()
            .into_iter()
            .filter(|t| t.starts_with(prefix))
            .collect();
        if last.as_ref() == Some(&now) {
            stable += 1;
            if stable >= 8 {
                return now;
            }
        } else {
            stable = 0;
            last = Some(now);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    last.unwrap()
}

/// The secondary indexes' tables: every `index-<UUID>` table but the `_id_`
/// index's of each collection (an index table of its own since phase 4d).
fn index_tables(st: &Storage) -> Vec<String> {
    let id_tables: BTreeSet<String> = st
        .list_collections("app")
        .unwrap()
        .iter()
        .filter_map(|c| st.index_table_name("app", c, "_id_").unwrap())
        .collect();
    tables_named(st, "table:index-")
        .into_iter()
        .filter(|t| !id_tables.contains(t))
        .collect()
}

fn collection_tables(st: &Storage) -> Vec<String> {
    tables_named(st, "table:collection-")
}

#[test]
fn an_index_is_a_table_of_its_own_named_as_mongod_names_one() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "a": 1}]);
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        st.create_index("app", "c", "a_u", &doc! {"a": 1}, &doc! {"unique": true})
            .unwrap();
        let table = st.index_table_name("app", "c", "a_1").unwrap().unwrap();
        let unique = st.index_table_name("app", "c", "a_u").unwrap().unwrap();
        for t in [&table, &unique] {
            let uuid = t.strip_prefix("table:index-").expect(t);
            let parts: Vec<_> = uuid.split('-').map(str::len).collect();
            assert_eq!(parts, [8, 4, 4, 4, 12], "{t}");
        }
        assert_ne!(table, unique);
        assert_eq!(index_tables(st), {
            let mut both = vec![table.clone(), unique.clone()];
            both.sort();
            both
        });
    });
}

#[test]
fn a_compound_index_of_mixed_directions_holds_keystring_entries() {
    with_db(|st| {
        insert(
            st,
            "c",
            &[
                doc! {"_id": 1, "a": 1, "b": "x"},
                doc! {"_id": 2, "a": 1i64, "b": "y"},
                doc! {"_id": 3, "a": 2.5, "b": "x"},
                doc! {"_id": 4, "a": Bson::Null, "b": 7},
            ],
        );
        st.create_index("app", "c", "a_1_b_-1", &doc! {"a": 1, "b": -1}, &doc! {})
            .unwrap();
        let want: BTreeSet<_> = documents(st, "c")
            .into_iter()
            .map(|(rid, d)| {
                expected_entry(
                    &[1, -1],
                    &[d.get("a").cloned().unwrap(), d.get("b").cloned().unwrap()],
                    rid,
                    None,
                )
            })
            .collect();
        let got = rows(st, "c", "a_1_b_-1");
        assert_eq!(got, want);
        // The TypeBits are the entry's value: empty when every value is the
        // default of its KeyString type (an int and a string), not otherwise.
        assert!(got.iter().any(|(_, tb)| tb.is_empty()));
        assert!(got.iter().any(|(_, tb)| !tb.is_empty()));
        // A write after the build keeps the same layout.
        insert(st, "c", &[doc! {"_id": 5, "a": 9, "b": "z"}]);
        let rid = documents(st, "c").last().unwrap().0;
        assert!(rows(st, "c", "a_1_b_-1").contains(&expected_entry(
            &[1, -1],
            &[Bson::Int32(9), Bson::String("z".into())],
            rid,
            None
        )));
    });
}

#[test]
fn a_multikey_index_holds_an_entry_per_element() {
    with_db(|st| {
        insert(
            st,
            "c",
            &[
                doc! {"_id": 1, "t": [3, 1, 3]},
                doc! {"_id": 2, "t": [2]},
                doc! {"_id": 3, "t": 9},
            ],
        );
        st.create_index("app", "c", "t_1", &doc! {"t": 1}, &doc! {})
            .unwrap();
        let mut want = BTreeSet::new();
        for (rid, d) in documents(st, "c") {
            match d.get("t").unwrap() {
                // One entry per distinct element, and the whole array: the
                // key this store writes besides mongod's, so that an
                // equality to the array finds it.
                Bson::Array(items) => {
                    let mut seen = Vec::new();
                    for v in items {
                        if !seen.contains(v) {
                            seen.push(v.clone());
                            want.insert(expected_entry(&[1], std::slice::from_ref(v), rid, None));
                        }
                    }
                    want.insert(expected_entry(
                        &[1],
                        &[Bson::Array(items.clone())],
                        rid,
                        None,
                    ));
                }
                v => {
                    want.insert(expected_entry(&[1], std::slice::from_ref(v), rid, None));
                }
            }
        }
        assert_eq!(rows(st, "c", "t_1"), want);
        assert!(st.index_is_multikey("app", "c", "t_1"));
    });
}

#[test]
fn a_collated_index_keys_its_strings_by_the_collations_sort_key() {
    with_db(|st| {
        let spec = doc! {"locale": "en", "strength": 2};
        insert(
            st,
            "c",
            &[
                doc! {"_id": 1, "s": "Apple"},
                doc! {"_id": 2, "s": "apple"},
                doc! {"_id": 3, "s": "Banana"},
            ],
        );
        st.create_index(
            "app",
            "c",
            "s_ci",
            &doc! {"s": 1},
            &doc! {"collation": spec.clone()},
        )
        .unwrap();
        let c = collation::parse(&spec).unwrap();
        let transform = |s: &str| collation::sort_key(s, &c);
        let want: BTreeSet<_> = documents(st, "c")
            .into_iter()
            .map(|(rid, d)| {
                expected_entry(&[1], &[d.get("s").cloned().unwrap()], rid, Some(&transform))
            })
            .collect();
        assert_eq!(rows(st, "c", "s_ci"), want);
        // "Apple" and "apple" are one key under strength 2: their entries
        // differ only by RecordId.
        let keys: Vec<_> = st.index_entries("app", "c", "s_ci").unwrap();
        assert_eq!(keys[0].0, keys[1].0);
        assert_ne!(keys[0].1, keys[1].1);
        // Read back through the explain stream's door, an entry decodes to
        // the sort-key encoding of its collation key, as the shared table
        // held it.
        let walk = st
            .index_entries_from("app", "c", "s_ci", IndexPos::Edge, None, 10, false)
            .unwrap();
        let apple =
            secantus_core::sortkey::encode_value(&Bson::String("Apple".into()), Some(&c)).unwrap();
        assert_eq!(walk.len(), 3);
        assert_eq!(walk[0].raw, apple);
        assert_eq!(walk[1].raw, apple);
    });
}

#[test]
fn a_partial_index_holds_entries_only_for_the_documents_it_covers() {
    with_db(|st| {
        insert(
            st,
            "c",
            &[
                doc! {"_id": 1, "a": 3},
                doc! {"_id": 2, "a": 7},
                doc! {"_id": 3, "a": 9},
            ],
        );
        st.create_index(
            "app",
            "c",
            "a_p",
            &doc! {"a": 1},
            &doc! {"partialFilterExpression": {"a": {"$gt": 5}}},
        )
        .unwrap();
        let want: BTreeSet<_> = documents(st, "c")
            .into_iter()
            .filter(|(_, d)| d.get_i32("a").unwrap() > 5)
            .map(|(rid, d)| expected_entry(&[1], &[d.get("a").cloned().unwrap()], rid, None))
            .collect();
        assert_eq!(want.len(), 2);
        assert_eq!(rows(st, "c", "a_p"), want);
        // A write into the filter's range adds its entry; out of it, none.
        insert(
            st,
            "c",
            &[doc! {"_id": 4, "a": 1}, doc! {"_id": 5, "a": 20}],
        );
        assert_eq!(rows(st, "c", "a_p").len(), 3);
    });
}

#[test]
fn the_catalog_names_each_index_table() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "a": 1, "b": 2}]);
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        st.create_index("app", "c", "b_1", &doc! {"b": 1}, &doc! {"unique": true})
            .unwrap();
        let entry = st.mdb_catalog_entry("app", "c").unwrap().unwrap();
        let idents = entry.get_document("idxIdent").unwrap();
        for name in ["a_1", "b_1"] {
            let table = st.index_table_name("app", "c", name).unwrap().unwrap();
            assert_eq!(
                format!("table:{}", idents.get_str(name).unwrap()),
                table,
                "{name}"
            );
        }
        // Dropping an index takes it out of the row.
        st.drop_index("app", "c", "a_1").unwrap();
        let entry = st.mdb_catalog_entry("app", "c").unwrap().unwrap();
        let idents = entry.get_document("idxIdent").unwrap();
        assert!(idents.get("a_1").is_none());
        assert!(idents.get("b_1").is_some());
    });
}

#[test]
fn dropping_an_index_or_its_collection_drops_the_table() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "a": 1, "b": 2}]);
        let base = collection_tables(st);
        assert_eq!(base.len(), 1);
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        st.create_index("app", "c", "b_1", &doc! {"b": 1}, &Document::new())
            .unwrap();
        let keep = st.index_table_name("app", "c", "b_1").unwrap().unwrap();
        assert_eq!(index_tables(st).len(), 2);
        st.drop_index("app", "c", "a_1").unwrap();
        assert_eq!(index_tables(st), vec![keep]);
        // The collection goes with every table it owns.
        assert!(st.drop_collection("app", "c").unwrap());
        assert!(index_tables(st).is_empty());
        assert!(collection_tables(st).is_empty());
        // Its `_id` index's table included.
        assert!(tables_named(st, "table:index-").is_empty());
    });
}

#[test]
fn a_rename_keeps_the_index_tables() {
    with_db(|st| {
        insert(
            st,
            "old",
            &[doc! {"_id": 1, "a": 5}, doc! {"_id": 2, "a": 6}],
        );
        st.create_index("app", "old", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let table = st.index_table_name("app", "old", "a_1").unwrap().unwrap();
        let before = rows(st, "old", "a_1");
        let (renamed, _) = st
            .rename_collection("app", "old", "app", "new", false)
            .unwrap();
        assert!(renamed);
        // Catalog-only: the same table, the same entries, under the new name.
        assert_eq!(
            st.index_table_name("app", "new", "a_1").unwrap().unwrap(),
            table
        );
        assert_eq!(rows(st, "new", "a_1"), before);
        assert_eq!(index_tables(st), vec![table]);
        assert_eq!(
            st.find_matching("app", "new", &doc! {"a": 6})
                .unwrap()
                .len(),
            1
        );
        let entry = st.mdb_catalog_entry("app", "new").unwrap().unwrap();
        assert!(entry.get_document("idxIdent").unwrap().get("a_1").is_some());
        assert!(st.mdb_catalog_entry("app", "old").unwrap().is_none());
    });
}

#[test]
fn a_rolled_back_index_leaves_no_table_and_a_committed_one_stays() {
    with_db(|st| {
        insert(st, "c", &[doc! {"_id": 1, "a": 5}]);
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
        })
        .unwrap()
        .unwrap();
        // Made in the transaction: nobody else lists it.
        assert_eq!(index_tables(st).len(), 1);
        st.rollback_user_transaction(&mut txn).unwrap();
        assert!(index_tables(st).is_empty());
        assert!(st.index_table_name("app", "c", "a_1").unwrap().is_none());

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
        })
        .unwrap()
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(index_tables(st).len(), 1);
        assert!(st.index_table_name("app", "c", "a_1").unwrap().is_some());
        assert_eq!(rows(st, "c", "a_1").len(), 1);
    });
}

/// A collection and an index made in one transaction (`deferred`: the
/// MongoDB server's, which holds the collection until the commit), then
/// rolled back or committed.
fn collection_and_index_in_a_transaction(st: &Storage, deferred: bool, commit: bool) {
    let mut txn = st.begin_user_transaction().unwrap();
    txn.set_defer_collection_creates(deferred);
    st.with_user_transaction(&mut txn, || {
        st.insert("app", "n", vec![enc(&doc! {"_id": 1, "a": 5})], true)
            .unwrap();
        st.create_index("app", "n", "a_1", &doc! {"a": 1}, &Document::new())
    })
    .unwrap()
    .unwrap();
    if commit {
        st.commit_user_transaction(&mut txn).unwrap();
    } else {
        st.rollback_user_transaction(&mut txn).unwrap();
    }
}

#[test]
fn an_index_made_with_its_collection_in_a_deferred_create_lives_or_dies_with_it() {
    with_db(|st| {
        collection_and_index_in_a_transaction(st, true, false);
        assert!(index_tables(st).is_empty());
        assert!(collection_tables(st).is_empty());

        collection_and_index_in_a_transaction(st, true, true);
        let table = st.index_table_name("app", "n", "a_1").unwrap().unwrap();
        assert_eq!(index_tables(st), vec![table]);
        assert_eq!(collection_tables(st).len(), 1);
        let entry = st.mdb_catalog_entry("app", "n").unwrap().unwrap();
        assert!(entry.get_document("idxIdent").unwrap().get("a_1").is_some());
        assert_eq!(rows(st, "n", "a_1").len(), 1);
    });
}

#[test]
fn an_index_made_with_its_collection_in_an_immediate_create_lives_or_dies_with_it() {
    with_db(|st| {
        collection_and_index_in_a_transaction(st, false, false);
        assert!(index_tables(st).is_empty());
        assert!(st.index_table_name("app", "n", "a_1").unwrap().is_none());

        collection_and_index_in_a_transaction(st, false, true);
        let table = st.index_table_name("app", "n", "a_1").unwrap().unwrap();
        assert_eq!(index_tables(st), vec![table]);
        assert_eq!(rows(st, "n", "a_1").len(), 1);
    });
}

#[test]
fn a_store_opened_again_drops_an_index_table_no_placement_names() {
    let home = temp_home();
    let stray;
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        insert(&st, "c", &[doc! {"_id": 1, "a": 5}]);
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        st.close().unwrap();
    }
    {
        // A crash between making a table and committing the index that
        // lists it leaves a table nothing names: made here by hand.
        let config = secantus_storage::store_format::mongod_log_config(
            &secantus_storage::wt_config("256M", 1000, false, "10MB"),
        );
        let conn = secantus_wt::Connection::open(home.to_str().unwrap(), &config).unwrap();
        let session = conn.open_session().unwrap();
        stray = "table:index-00000000-0000-4000-8000-000000000000".to_string();
        session
            .create(
                &stray,
                "key_format=u,value_format=u,app_metadata=(formatVersion=8)",
            )
            .unwrap();
        drop(session);
        drop(conn);
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let tables = index_tables(&st);
    assert_eq!(tables.len(), 1, "{tables:?}");
    assert!(!tables.contains(&stray));
    assert_eq!(
        st.find_matching("app", "c", &doc! {"a": 5}).unwrap().len(),
        1
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
