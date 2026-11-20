//! Uniqueness lives in the index tables (storage-model plan, phase 4e), as
//! mongod's does for an index whose entries carry the RecordId in their key:
//! an insert claims the KeyString of its value alone (inserted with
//! `overwrite=false`, so WiredTiger itself refuses it to a concurrent writer
//! of the value, and removed again in the same transaction) and then looks for
//! an entry that starts with it under another RecordId. There is no table of
//! claims beside the indexes.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-uniqtab-{}-{}", std::process::id(), n));
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

fn insert_one(st: &Storage, d: &Document) -> Result<Vec<u8>, StorageError> {
    st.insert_one("app", "c", &enc(d))
}

fn duplicate(r: Result<Vec<u8>, StorageError>) -> secantus_storage::UniqueConflict {
    match r {
        Err(StorageError::DuplicateKey(c)) => *c,
        other => panic!("expected DuplicateKey, got {other:?}"),
    }
}

#[test]
fn a_unique_index_is_a_formatversion_14_table_holding_its_entries_only() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
        insert_one(st, &doc! {"_id": 2, "e": "y"}).unwrap();
        // The claim's prefix key is gone with the transaction that took it:
        // the table holds one entry per document, each key ending in its
        // RecordId, and nothing shorter.
        let rows = st.index_table_rows("app", "c", "e_1").unwrap();
        assert_eq!(rows.len(), 2);
        for (key, _) in &rows {
            let (_, rid) = secantus_core::keystring::without_record_id_long_at_end(key).unwrap();
            assert!(rid == 1 || rid == 2, "{rid}");
        }
        // A rejected insert leaves nothing behind either.
        duplicate(insert_one(st, &doc! {"_id": 3, "e": "x"}));
        assert_eq!(st.index_table_rows("app", "c", "e_1").unwrap().len(), 2);
        let names = st.wt_table_names().unwrap();
        assert!(!names.iter().any(|n| n == "table:secantus_unique_keys"));
    });
}

#[test]
fn a_duplicate_is_refused_with_mongods_error() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
        let c = duplicate(insert_one(st, &doc! {"_id": 2, "e": "x"}));
        assert_eq!(c.namespace, "app.c");
        assert_eq!(c.index, "e_1");
        assert_eq!(c.key_pattern, doc! {"e": 1});
        assert_eq!(c.key_value, doc! {"e": "x"});
        assert_eq!(
            c.errmsg(),
            "E11000 duplicate key error collection: app.c index: e_1 dup key: { e: \"x\" }"
        );
        // As a write error of a batch, with mongod's fields.
        let (n, errors) = st
            .insert("app", "c", vec![enc(&doc! {"_id": 4, "e": "x"})], true)
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(errors[0].get_i32("code"), Ok(11000));
        assert_eq!(errors[0].get_document("keyPattern"), Ok(&doc! {"e": 1}));
        assert_eq!(errors[0].get_document("keyValue"), Ok(&doc! {"e": "x"}));
    });
}

#[test]
fn equal_numbers_of_different_types_are_one_value() {
    with_db(|st| {
        st.create_index("app", "c", "n_1", &doc! {"n": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "n": 1}).unwrap();
        duplicate(insert_one(st, &doc! {"_id": 2, "n": 1.0}));
        duplicate(insert_one(st, &doc! {"_id": 3, "n": 1i64}));
    });
}

#[test]
fn a_document_may_repeat_a_value_of_its_own_and_an_update_may_keep_it() {
    with_db(|st| {
        st.create_index("app", "c", "t_1", &doc! {"t": 1}, &doc! {"unique": true})
            .unwrap();
        // One document, the same element twice: one key's worth.
        insert_one(st, &doc! {"_id": 1, "t": [1, 1, 2]}).unwrap();
        duplicate(insert_one(st, &doc! {"_id": 2, "t": [2, 3]}));
        // An update that leaves the key where it is is not a duplicate of itself.
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"u": 1}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
        // An update onto another's value is.
        insert_one(st, &doc! {"_id": 3, "t": [9]}).unwrap();
        let r = st.update_matching(
            "app",
            "c",
            &doc! {"_id": 3},
            &doc! {"$set": {"t": [1]}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        );
        assert!(matches!(r, Err(StorageError::DuplicateKey(_))), "{r:?}");
        // The value it left is free.
        st.update_matching(
            "app",
            "c",
            &doc! {"_id": 1},
            &doc! {"$set": {"t": [5]}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )
        .unwrap();
        insert_one(st, &doc! {"_id": 4, "t": [1]}).unwrap();
    });
}

#[test]
fn null_and_a_missing_field_are_one_value_unless_the_index_is_sparse() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1}).unwrap();
        duplicate(insert_one(st, &doc! {"_id": 2, "e": Bson::Null}));
        duplicate(insert_one(st, &doc! {"_id": 3}));
        st.create_index(
            "app",
            "c",
            "s_1",
            &doc! {"s": 1},
            &doc! {"unique": true, "sparse": true},
        )
        .unwrap();
        // A sparse unique index holds no entry for a document without the field.
        insert_one(st, &doc! {"_id": 4, "e": 4}).unwrap();
        insert_one(st, &doc! {"_id": 5, "e": 5}).unwrap();
        insert_one(st, &doc! {"_id": 6, "e": 6, "s": 1}).unwrap();
        duplicate(insert_one(st, &doc! {"_id": 7, "e": 7, "s": 1}));
    });
}

#[test]
fn a_partial_unique_index_holds_only_the_documents_it_covers() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "p_1",
            &doc! {"p": 1},
            &doc! {"unique": true, "partialFilterExpression": {"keep": true}},
        )
        .unwrap();
        insert_one(st, &doc! {"_id": 1, "p": 1, "keep": true}).unwrap();
        // Outside the filter: neither held nor refused.
        insert_one(st, &doc! {"_id": 2, "p": 1, "keep": false}).unwrap();
        insert_one(st, &doc! {"_id": 3, "p": 1}).unwrap();
        // Inside it: the value is taken.
        duplicate(insert_one(st, &doc! {"_id": 4, "p": 1, "keep": true}));
        assert_eq!(st.index_table_rows("app", "c", "p_1").unwrap().len(), 1);
        // A document that moves into the filter takes the value or is refused.
        let r = st.update_matching(
            "app",
            "c",
            &doc! {"_id": 2},
            &doc! {"$set": {"keep": true}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        );
        assert!(matches!(r, Err(StorageError::DuplicateKey(_))), "{r:?}");
    });
}

#[test]
fn a_collated_unique_index_calls_equal_what_its_collation_does() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "s_1",
            &doc! {"s": 1},
            &doc! {"unique": true, "collation": {"locale": "en", "strength": 2}},
        )
        .unwrap();
        insert_one(st, &doc! {"_id": 1, "s": "abc"}).unwrap();
        let c = duplicate(insert_one(st, &doc! {"_id": 2, "s": "ABC"}));
        assert_eq!(c.index, "s_1");
        assert!(c.collation.is_some());
        insert_one(st, &doc! {"_id": 3, "s": "abd"}).unwrap();
        assert_eq!(st.index_table_rows("app", "c", "s_1").unwrap().len(), 2);
    });
}

#[test]
fn prepare_unique_refuses_new_duplicates_and_tolerates_old_ones() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "x": 1}).unwrap();
        insert_one(st, &doc! {"_id": 2, "x": 1}).unwrap();
        assert!(st
            .set_index_options("app", "c", "x_1", &doc! {"prepareUnique": true})
            .unwrap());
        duplicate(insert_one(st, &doc! {"_id": 3, "x": 1}));
        // The two that were there stay, and a new value is fine.
        insert_one(st, &doc! {"_id": 4, "x": 2}).unwrap();
        assert_eq!(st.index_table_rows("app", "c", "x_1").unwrap().len(), 3);
    });
}

#[test]
fn a_deleted_document_frees_its_value() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
        assert!(st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap());
        insert_one(st, &doc! {"_id": 2, "e": "x"}).unwrap();
    });
}

#[test]
fn a_dropped_collection_leaves_no_claim_to_refuse_its_successor() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
        assert!(st.drop_collection("app", "c").unwrap());
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
    });
}

#[test]
fn a_renamed_collection_keeps_its_unique_entries() {
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
        assert_eq!(
            st.rename_collection("app", "c", "app", "d", false).unwrap(),
            (true, None)
        );
        match st.insert_one("app", "d", &enc(&doc! {"_id": 2, "e": "x"})) {
            Err(StorageError::DuplicateKey(c)) => assert_eq!(c.namespace, "app.d"),
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
        // And the name it left holds nothing.
        insert_one(st, &doc! {"_id": 1, "e": "x"}).unwrap();
    });
}

#[test]
fn two_transactions_inserting_one_value_cannot_both_commit() {
    // Neither sees the other's uncommitted entry, so the read of the entries
    // alone would let both through: the prefix key each takes is what makes
    // WiredTiger refuse the second.
    with_db(|st| {
        st.create_index("app", "c", "e_1", &doc! {"e": 1}, &doc! {"unique": true})
            .unwrap();
        insert_one(st, &doc! {"_id": 0, "e": "seed"}).unwrap();
        let mut a = st.begin_user_transaction().unwrap();
        let mut b = st.begin_user_transaction().unwrap();
        let ra = st
            .with_user_transaction(&mut a, || insert_one(st, &doc! {"_id": 1, "e": "x"}))
            .unwrap();
        assert!(ra.is_ok(), "{ra:?}");
        let rb = st
            .with_user_transaction(&mut b, || insert_one(st, &doc! {"_id": 2, "e": "x"}))
            .unwrap();
        // The second writer meets the first's uncommitted claim.
        assert!(
            matches!(
                rb,
                Err(StorageError::WriteConflict | StorageError::DuplicateKey(_))
            ),
            "{rb:?}"
        );
        st.rollback_user_transaction(&mut b).unwrap();
        st.commit_user_transaction(&mut a).unwrap();
        duplicate(insert_one(st, &doc! {"_id": 3, "e": "x"}));
    });
}
