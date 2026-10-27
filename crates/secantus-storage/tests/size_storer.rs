//! The size storer (RM-191): every collection's record count, data size and
//! index sizes, kept by the writes rather than counted when asked. Each test
//! changes a collection one way and checks the kept numbers against a walk
//! of every document and index entry (`counted_collection_sizes`), which is
//! what `count`, `collStats` and `dbStats` read before the size storer.
//! Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{CollectionSizes, Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-sizes-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn durable(home: &std::path::Path) -> Storage {
    Storage::open_with_options(
        home.to_str().unwrap(),
        &StorageOptions {
            durable: Some(true),
            ..StorageOptions::default()
        },
    )
    .unwrap()
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

/// The kept sizes of `db.coll`, after checking them against a walk.
#[track_caller]
fn kept(st: &Storage, db: &str, coll: &str) -> CollectionSizes {
    let kept = st.collection_sizes(db, coll).unwrap();
    let counted = st.counted_collection_sizes(db, coll).unwrap();
    assert_eq!(kept, counted, "{db}.{coll}: kept sizes differ from a walk");
    kept
}

fn update(st: &Storage, coll: &str, filter: Document, spec: Document, multi: bool, upsert: bool) {
    st.update_matching(
        "app",
        coll,
        &filter,
        &spec,
        multi,
        upsert,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
    .unwrap();
}

#[test]
fn inserts_updates_and_deletes_move_the_counts() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let docs: Vec<Vec<u8>> = (0..50)
            .map(|i| enc(&doc! {"_id": i, "a": i % 7, "pad": "x".repeat(i as usize)}))
            .collect();
        st.insert("app", "c", docs, true).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"a": [1, 1, 2, 3]}))
            .unwrap();
        let s = kept(st, "app", "c");
        assert_eq!(s.records, 51);
        assert!(s.index_sizes.get("a_1").is_some_and(|b| *b > 0));

        // An unordered batch with duplicates: only the new documents count.
        let docs: Vec<Vec<u8>> = (45..60).map(|i| enc(&doc! {"_id": i})).collect();
        let (n, errors) = st.insert("app", "c", docs, false).unwrap();
        assert_eq!((n, errors.len()), (10, 5));
        assert_eq!(kept(st, "app", "c").records, 61);

        // Grow, shrink, and change indexed values.
        update(
            st,
            "c",
            doc! {"_id": 1},
            doc! {"$set": {"pad": "y".repeat(500)}},
            false,
            false,
        );
        update(
            st,
            "c",
            doc! {"_id": 40},
            doc! {"$unset": {"pad": 1}},
            false,
            false,
        );
        update(
            st,
            "c",
            doc! {"_id": {"$type": "int"}},
            doc! {"$inc": {"a": 1}},
            true,
            false,
        );
        update(
            st,
            "c",
            doc! {"_id": 99},
            doc! {"$set": {"a": 5}},
            false,
            true,
        );
        st.replace_by_id(
            "app",
            "c",
            &Bson::Int32(2),
            &enc(&doc! {"b": "z".repeat(90)}),
        )
        .unwrap();
        assert_eq!(kept(st, "app", "c").records, 62);

        st.delete_by_id("app", "c", &Bson::Int32(3)).unwrap();
        st.delete_matching(
            "app",
            "c",
            &doc! {"_id": {"$lt": 10}},
            1,
            &Document::new(),
            None,
        )
        .unwrap();
        st.delete_matching(
            "app",
            "c",
            &doc! {"a": {"$gte": 4}},
            0,
            &Document::new(),
            None,
        )
        .unwrap();
        kept(st, "app", "c");
        st.delete_matching("app", "c", &doc! {}, 0, &Document::new(), None)
            .unwrap();
        let s = kept(st, "app", "c");
        assert_eq!(s, CollectionSizes::default());
    });
}

#[test]
fn index_builds_drops_and_geo_entries_are_counted() {
    with_db(|st| {
        let docs: Vec<Vec<u8>> = (0..30)
            .map(|i| {
                enc(&doc! {
                    "_id": i,
                    "tags": ["a", "b", "a"],
                    "loc": {"type": "MultiPoint", "coordinates": [[i as f64 * 0.001, 0.0], [i as f64 * 0.001, 0.0001]]},
                })
            })
            .collect();
        st.insert("app", "g", docs, true).unwrap();
        st.create_index("app", "g", "tags_1", &doc! {"tags": 1}, &Document::new())
            .unwrap();
        st.create_index(
            "app",
            "g",
            "loc_2dsphere",
            &doc! {"loc": "2dsphere"},
            &Document::new(),
        )
        .unwrap();
        let s = kept(st, "app", "g");
        assert!(s.index_sizes.contains_key("loc_2dsphere"));
        st.insert_one(
            "app",
            "g",
            &enc(&doc! {"_id": 100, "tags": ["c", "c"], "loc": {"type": "Point", "coordinates": [1.0, 1.0]}}),
        )
        .unwrap();
        st.delete_by_id("app", "g", &Bson::Int32(5)).unwrap();
        kept(st, "app", "g");
        st.drop_index("app", "g", "tags_1").unwrap();
        let s = kept(st, "app", "g");
        assert!(!s.index_sizes.contains_key("tags_1"));
        st.drop_all_indexes("app", "g").unwrap();
        let s = kept(st, "app", "g");
        assert!(s.index_sizes.is_empty());
    });
}

#[test]
fn capped_eviction_and_ttl_are_counted() {
    with_db(|st| {
        st.create_collection_with_options(
            "app",
            "cap",
            &doc! {"capped": true, "size": 4096, "max": 3},
        )
        .unwrap();
        for i in 0..10 {
            st.insert(
                "app",
                "cap",
                vec![enc(&doc! {"i": i, "p": "p".repeat(50)})],
                true,
            )
            .unwrap();
        }
        assert_eq!(kept(st, "app", "cap").records, 3);
        let docs: Vec<Vec<u8>> = (0..10).map(|i| enc(&doc! {"j": i})).collect();
        st.insert("app", "cap", docs, true).unwrap();
        assert_eq!(kept(st, "app", "cap").records, 3);

        st.create_index(
            "app",
            "ttl",
            "at_1",
            &doc! {"at": 1},
            &doc! {"expireAfterSeconds": 0},
        )
        .unwrap();
        let old = bson::DateTime::from_millis(1_000);
        let docs: Vec<Vec<u8>> = (0..5).map(|i| enc(&doc! {"_id": i, "at": old})).collect();
        st.insert("app", "ttl", docs, true).unwrap();
        st.insert_one("app", "ttl", &enc(&doc! {"_id": 9})).unwrap();
        st.prune_ttl("app", "ttl", bson::DateTime::now()).unwrap();
        assert_eq!(kept(st, "app", "ttl").records, 1);
    });
}

#[test]
fn rename_and_drop_move_and_clear_the_counts() {
    with_db(|st| {
        st.create_index("app", "src", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let docs: Vec<Vec<u8>> = (0..20).map(|i| enc(&doc! {"_id": i, "a": i})).collect();
        st.insert("app", "src", docs, true).unwrap();
        let before = kept(st, "app", "src");
        st.insert_one("app", "dst", &enc(&doc! {"_id": "old"}))
            .unwrap();
        st.rename_collection("app", "src", "app", "dst", true)
            .unwrap();
        assert_eq!(kept(st, "app", "src"), CollectionSizes::default());
        assert_eq!(kept(st, "app", "dst"), before);

        // A rename inside a user transaction, rolled back, then committed.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.rename_collection("app", "dst", "app", "moved", false)
        })
        .unwrap()
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
        assert_eq!(kept(st, "app", "dst"), before);
        assert_eq!(kept(st, "app", "moved"), CollectionSizes::default());

        st.drop_collection("app", "dst").unwrap();
        assert_eq!(kept(st, "app", "dst"), CollectionSizes::default());

        st.insert_one("other", "a", &enc(&doc! {"x": 1})).unwrap();
        st.insert_one("other", "b", &enc(&doc! {"x": 2})).unwrap();
        st.drop_database("other").unwrap();
        assert_eq!(kept(st, "other", "a"), CollectionSizes::default());
        assert_eq!(kept(st, "other", "b"), CollectionSizes::default());
    });
}

/// mongod's fast count counts a transaction's writes as they are made, and
/// takes them back when it aborts (measured 8.2.11: a `count` answered 3 for
/// 2 committed documents and one an open transaction inserted).
#[test]
fn a_transactions_writes_count_until_it_aborts() {
    with_db(|st| {
        st.insert_one("app", "t", &enc(&doc! {"_id": 1, "a": "x"}))
            .unwrap();
        st.insert_one("app", "t", &enc(&doc! {"_id": 2, "a": "x"}))
            .unwrap();
        let committed = kept(st, "app", "t");

        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "t", &enc(&doc! {"_id": 3, "a": "y".repeat(40)}))
                .unwrap();
            st.delete_by_id("app", "t", &Bson::Int32(1)).unwrap();
            update(
                st,
                "t",
                doc! {"_id": 2},
                doc! {"$set": {"a": "z".repeat(30)}},
                false,
                false,
            );
        })
        .unwrap();
        // Another reader: the uncommitted changes are in the kept numbers,
        // not in what a walk (a snapshot) reads.
        let during = st.collection_sizes("app", "t").unwrap();
        assert_eq!(during.records, 2);
        assert!(during.data_size > committed.data_size);
        assert_eq!(st.counted_collection_sizes("app", "t").unwrap(), committed);
        st.rollback_user_transaction(&mut txn).unwrap();
        assert_eq!(kept(st, "app", "t"), committed);

        // The same transaction, committed.
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "t", &enc(&doc! {"_id": 3, "a": "y".repeat(40)}))
                .unwrap();
            st.delete_by_id("app", "t", &Bson::Int32(1)).unwrap();
        })
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        assert_eq!(kept(st, "app", "t").records, 2);

        // A drop inside a transaction, rolled back.
        let before = kept(st, "app", "t");
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || st.drop_collection("app", "t"))
            .unwrap()
            .unwrap();
        assert_eq!(st.collection_sizes("app", "t").unwrap().records, 0);
        st.rollback_user_transaction(&mut txn).unwrap();
        assert_eq!(kept(st, "app", "t"), before);
    });
}

/// A clean close saves the counts and the next open reads them back
/// without walking the store.
#[test]
fn a_clean_close_keeps_the_counts_without_a_walk() {
    let home = temp_home();
    let expected = {
        let st = durable(&home);
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let docs: Vec<Vec<u8>> = (0..100).map(|i| enc(&doc! {"_id": i, "a": i})).collect();
        st.insert("app", "c", docs, true).unwrap();
        st.insert_one("app", "d", &enc(&doc! {"x": 1})).unwrap();
        let e = (kept(&st, "app", "c"), kept(&st, "app", "d"));
        st.close().unwrap();
        e
    };
    let st = durable(&home);
    assert_eq!(
        st.size_recounts(),
        0,
        "a clean close's counts were not trusted"
    );
    assert_eq!(kept(&st, "app", "c"), expected.0);
    assert_eq!(kept(&st, "app", "d"), expected.1);
    // And the open after that still trusts them.
    st.insert_one("app", "c", &enc(&doc! {"_id": 1000, "a": 1}))
        .unwrap();
    st.close().unwrap();
    drop(st);
    let st = durable(&home);
    assert_eq!(st.size_recounts(), 0);
    assert_eq!(kept(&st, "app", "c").records, 101);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A store that did not close cleanly has no counts to trust: the open
/// makes them again by one walk, as a mongod replica-set member's startup
/// recovery does (measured 8.2.11: 1,300 written, the process killed, 1,300
/// counted after the restart).
#[test]
fn an_unclean_shutdown_is_recounted_at_open() {
    const CHILD: &str = "SECANTUS_SIZES_CRASH_CHILD";
    if let Ok(home) = std::env::var(CHILD) {
        // The child: write, then end the process without closing the store.
        let st = durable(std::path::Path::new(&home));
        let docs: Vec<Vec<u8>> = (0..300)
            .map(|i| enc(&doc! {"_id": i, "p": "x".repeat(20)}))
            .collect();
        st.insert("app", "c", docs, true).unwrap();
        st.delete_matching(
            "app",
            "c",
            &doc! {"_id": {"$lt": 50}},
            0,
            &Document::new(),
            None,
        )
        .unwrap();
        // Make the writes durable, as a journal flush would.
        st.checkpoint_now().unwrap();
        std::process::exit(0);
    }
    let home = temp_home();
    {
        // A clean close first, so there IS a marker for the crash to beat.
        let st = durable(&home);
        st.insert_one("app", "c", &enc(&doc! {"_id": "kept"}))
            .unwrap();
        st.close().unwrap();
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_unclean_shutdown_is_recounted_at_open",
            "--test-threads=1",
        ])
        .env(CHILD, home.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    let st = durable(&home);
    assert_eq!(
        st.size_recounts(),
        1,
        "the unclean shutdown was not noticed"
    );
    assert_eq!(kept(&st, "app", "c").records, 251);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A foreground validation sets the counts to what it read, as mongod's
/// does (`updateStatsAfterRepair`).
#[test]
fn a_check_corrects_the_counts() {
    with_db(|st| {
        let docs: Vec<Vec<u8>> = (0..10).map(|i| enc(&doc! {"_id": i})).collect();
        st.insert("app", "v", docs, true).unwrap();
        let before = kept(st, "app", "v");
        st.check_collection("app", "v").unwrap();
        assert_eq!(kept(st, "app", "v"), before);
    });
}

/// The kept numbers are read without walking the collection: the same
/// number of walks for 2,000 documents as for none (RM-191).
#[test]
fn reading_the_counts_walks_nothing() {
    with_db(|st| {
        let docs: Vec<Vec<u8>> = (0..2_000).map(|i| enc(&doc! {"_id": i})).collect();
        st.insert("app", "big", docs, true).unwrap();
        let walks = secantus_storage::collection_walks();
        for _ in 0..100 {
            assert_eq!(st.collection_sizes("app", "big").unwrap().records, 2_000);
        }
        assert_eq!(secantus_storage::collection_walks(), walks);
    });
}
