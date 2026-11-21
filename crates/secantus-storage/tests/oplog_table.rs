//! `local.oplog.rs` is a real capped collection in a WiredTiger table of its
//! own (phase 4b-1 of `tasks/mongod-storage-model-plan.md`): mongod 8.2.11's
//! table config, its `_mdb_catalog` row, and a RecordId that is the entry's
//! `ts`. The fixture it is held to is
//! `tests/fixtures/storage_model/mongod-8.2.11-oplog.json`.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-oplogtab-{}-{}", std::process::id(), n));
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

fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/storage_model/mongod-8.2.11-oplog.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

#[test]
fn the_oplog_is_one_collection_table_with_mongods_config() {
    // Logged is the default; pinned here so the non-logged oplog lane
    // (SECANTUS_OPLOG_NONLOGGED=1) still checks it.
    let home = temp_home();
    let st = Storage::open_with_options(
        home.to_str().unwrap(),
        &secantus_storage::StorageOptions {
            oplog_nonlogged: Some(false),
            ..secantus_storage::StorageOptions::default()
        },
    )
    .unwrap();
    {
        let uri = st.oplog_table().to_string();
        assert!(uri.starts_with("table:collection-"), "{uri}");
        let config = st.table_file_config(&uri).unwrap().expect("a table");
        // The fixture's own config string, mongod 8.2.11's.
        let mongod = fixture()["table"]["config"].as_str().unwrap().to_string();
        for part in [
            "key_format=q",
            "value_format=u",
            "block_compressor=snappy",
            "app_metadata=(formatVersion=1,oplogKeyExtractionVersion=1)",
            "log=(enabled=true)",
            "memory_page_max=10m",
            "split_pct=90",
            "leaf_value_max=64MB",
            "checksum=on",
        ] {
            assert!(mongod.contains(part), "the fixture lacks {part}");
            assert!(config.contains(part), "{part} missing from {config}");
        }
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_catalog_row_is_mongods() {
    with_db(|st| {
        let entry = st.mdb_catalog_entry("local", "oplog.rs").unwrap().unwrap();
        assert_eq!(
            entry.keys().collect::<Vec<_>>(),
            ["md", "idxIdent", "ident", "ns"]
        );
        assert_eq!(entry.get_str("ns").unwrap(), "local.oplog.rs");
        assert_eq!(
            format!("table:{}", entry.get_str("ident").unwrap()),
            st.oplog_table()
        );
        assert!(entry.get_document("idxIdent").unwrap().is_empty());
        let md = entry.get_document("md").unwrap();
        assert_eq!(md.get_str("ns").unwrap(), "local.oplog.rs");
        assert!(md.get_array("indexes").unwrap().is_empty());
        let options = md.get_document("options").unwrap();
        // mongod's `{uuid, capped, size, autoIndexId}`, in its order.
        assert_eq!(
            options.keys().collect::<Vec<_>>(),
            ["uuid", "capped", "size", "autoIndexId"]
        );
        assert_eq!(options.get_bool("capped"), Ok(true));
        assert_eq!(options.get_bool("autoIndexId"), Ok(false));
        assert!(matches!(
            options.get("size"),
            Some(Bson::Int32(_) | Bson::Int64(_))
        ));
        let mongod = &fixture()["catalog_entry"];
        assert_eq!(
            mongod["md"]["options"]["capped"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(mongod["md"]["indexes"], serde_json::json!([]));
    });
}

#[test]
fn every_row_is_keyed_by_its_timestamp() {
    with_db(|st| {
        for i in 0..20 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"x": 1}))
            .unwrap();
        st.delete_by_id("app", "c", &Bson::Int32(2)).unwrap();
        let rows = st.read_oplog(1, 1000).unwrap();
        assert!(rows.len() >= 22);
        for (key, blob) in &rows {
            let ts = Document::from_reader(&mut std::io::Cursor::new(blob))
                .unwrap()
                .get_timestamp("ts")
                .unwrap();
            assert_eq!(*key, (i64::from(ts.time) << 32) | i64::from(ts.increment));
        }
        // And the keys ascend: the table is the oplog's order.
        assert!(rows.windows(2).all(|w| w[0].0 < w[1].0));
    });
}

#[test]
fn a_tailable_scan_of_the_oplog_sees_a_new_entry() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        let first = st
            .scan_docs_after_recordid("local", "oplog.rs", None)
            .unwrap();
        assert!(!first.is_empty());
        let after = first.last().unwrap().0;
        assert!(st
            .scan_docs_after_recordid("local", "oplog.rs", Some(after))
            .unwrap()
            .is_empty());
        assert_eq!(
            st.collection_max_recordid("local", "oplog.rs").unwrap(),
            Some(st.read_oplog(1, 1000).unwrap().last().unwrap().0)
        );
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        let more = st
            .scan_docs_after_recordid("local", "oplog.rs", Some(after))
            .unwrap();
        assert_eq!(more.len(), 1);
        let e = Document::from_reader(&mut std::io::Cursor::new(&more[0].1)).unwrap();
        assert_eq!(e.get_str("op"), Ok("i"));
        assert_eq!(e.get_document("o2").unwrap().get_i32("_id"), Ok(2));
        assert!(more[0].0 > after);
    });
}

#[test]
fn the_oplogs_uuid_is_the_catalogs_and_it_is_not_a_registry_collection() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        let entry = st.mdb_catalog_entry("local", "oplog.rs").unwrap().unwrap();
        let uuid = match entry
            .get_document("md")
            .unwrap()
            .get_document("options")
            .unwrap()
            .get("uuid")
        {
            Some(Bson::Binary(b)) => b.bytes.clone(),
            other => panic!("no uuid: {other:?}"),
        };
        assert_eq!(st.collection_uuid("local", "oplog.rs").unwrap(), uuid);
        // Asking for it made no collection table of its own.
        assert!(!st
            .list_collections("local")
            .unwrap()
            .contains(&"oplog.rs".to_string()));
        assert!(st
            .collection_table_name("local", "oplog.rs")
            .unwrap()
            .is_none());
    });
}

/// A server's store (unlogged data tables over a logged oplog) that is killed
/// before its first checkpoint comes back with its oplog, and replays it. The
/// `_mdb_catalog` is not logged, so a catalog row written after the last
/// checkpoint is gone with the crash: the one that names the oplog's table is
/// written at the store's creation, which ends in a checkpoint, as mongod's
/// `replSetInitiate` does. Without that the store would make a new empty oplog,
/// drop the old as an orphan, and every acknowledged write would be lost.
#[test]
fn a_kill_before_the_first_checkpoint_keeps_the_oplog_and_replays_it() {
    const CHILD: &str = "SECANTUS_OPLOG_TABLE_CRASH_CHILD";
    let options = secantus_storage::StorageOptions {
        durable: Some(true),
        data_nonlogged: Some(true),
        oplog_nonlogged: Some(false),
        ..secantus_storage::StorageOptions::default()
    };
    if let Ok(home) = std::env::var(CHILD) {
        let st = Storage::open_with_options(&home, &options).unwrap();
        for i in 0..20 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        // An index built over them: logged as a two-phase build, and brought
        // back by replaying its `commitIndexBuild`.
        st.create_index("app", "c", "a_1", &doc! {"_id": 1}, &Document::new())
            .unwrap();
        std::fs::write(format!("{home}.uri"), st.oplog_table()).unwrap();
        // The journal on disk, as the flusher puts it; then the process ends
        // with no close and no checkpoint.
        st.flush_journal().unwrap();
        std::process::exit(0);
    }
    let home = temp_home();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_kill_before_the_first_checkpoint_keeps_the_oplog_and_replays_it",
            "--test-threads=1",
        ])
        .env(CHILD, home.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    let uri = std::fs::read_to_string(format!("{}.uri", home.to_str().unwrap())).unwrap();
    let st = Storage::open_with_options(home.to_str().unwrap(), &options).unwrap();
    assert_eq!(st.oplog_table(), uri, "the same table, not a new empty one");
    assert_eq!(
        st.find_matching("app", "c", &doc! {}).unwrap().len(),
        20,
        "every acknowledged write came back"
    );
    assert!(
        st.list_indexes("app", "c")
            .unwrap()
            .iter()
            .any(|ix| ix.get_str("name") == Ok("a_1")),
        "the replayed build's index"
    );
    assert!(st
        .find_matching("config", "system.indexBuilds", &doc! {})
        .unwrap()
        .is_empty());
    // The catalog names it, with the uuid it was created with.
    let entry = st.mdb_catalog_entry("local", "oplog.rs").unwrap().unwrap();
    assert_eq!(
        format!("table:{}", entry.get_str("ident").unwrap()),
        st.oplog_table()
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(format!("{}.uri", home.to_str().unwrap()));
}

/// A transaction too large for one entry is logged as mongod logs it
/// (`tests/fixtures/storage_model/mongod-8.2.11-oplog-batching.json`, twenty 1 MB
/// inserts): a chain of `applyOps` entries, the first `partialTxn: true` with
/// sixteen operations, the last `count: 20` with four, each after the one
/// before by `prevOpTime`.
#[test]
fn a_transaction_over_the_entry_limit_is_a_partial_txn_chain() {
    with_db(|st| {
        st.create_collection("app", "big").unwrap();
        let before = st.oplog_tail_seq();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            for i in 0..20 {
                st.insert_one(
                    "app",
                    "big",
                    &enc(&doc! {"_id": i, "p": "x".repeat(1_000_000)}),
                )
                .unwrap();
            }
        })
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        assert_eq!(rows.len(), 2, "two entries");
        let entries: Vec<Document> = rows
            .iter()
            .map(|(_, b)| bson::from_slice(b).unwrap())
            .collect();
        let ops = |e: &Document| {
            e.get_document("o")
                .unwrap()
                .get_array("applyOps")
                .unwrap()
                .len()
        };
        assert_eq!((ops(&entries[0]), ops(&entries[1])), (16, 4));
        assert_eq!(
            entries[0].get_document("o").unwrap().get_bool("partialTxn"),
            Ok(true)
        );
        assert_eq!(
            entries[1].get_document("o").unwrap().get_i32("count"),
            Ok(20)
        );
        let prev = |e: &Document| {
            e.get_document("prevOpTime")
                .unwrap()
                .get_timestamp("ts")
                .unwrap()
        };
        assert_eq!(
            prev(&entries[0]),
            bson::Timestamp {
                time: 0,
                increment: 0
            }
        );
        assert_eq!(prev(&entries[1]), entries[0].get_timestamp("ts").unwrap());
        assert!(rows[1].0 > rows[0].0);
        // All twenty are in the collection, and a replay reads all twenty.
        let unwound: usize = entries
            .iter()
            .map(|e| secantus_storage::oplog_entry::unwind(e).len())
            .sum();
        assert_eq!(unwound, 20);
    });
}

fn ops_of(rows: &[(i64, Vec<u8>)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(_, b)| {
            let e: Document = bson::from_slice(b).unwrap();
            let o = e.get_document("o").unwrap();
            let first = o.keys().next().cloned().unwrap_or_default();
            (
                e.get_str("op").unwrap().to_string(),
                format!("{} {}", e.get_str("ns").unwrap(), first),
            )
        })
        .collect()
}

/// An index on a collection that has documents is built in two phases, as
/// mongod logs it (the fixture's `createIndexes` step): the build's row into
/// `config.system.indexBuilds`, `startIndexBuild`, the row updated,
/// `commitIndexBuild`, the row deleted. The collection is a real one.
#[test]
fn an_index_build_is_logged_in_two_phases() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1}))
            .unwrap();
        let before = st.oplog_tail_seq();
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        assert_eq!(
            ops_of(&rows),
            vec![
                ("i".to_string(), "config.system.indexBuilds _id".to_string()),
                ("c".to_string(), "app.$cmd startIndexBuild".to_string()),
                ("u".to_string(), "config.system.indexBuilds $v".to_string()),
                ("c".to_string(), "app.$cmd commitIndexBuild".to_string()),
                ("d".to_string(), "config.system.indexBuilds _id".to_string()),
            ]
        );
        assert!(st
            .collection_exists("config", "system.indexBuilds")
            .unwrap());
        assert!(st
            .find_matching("config", "system.indexBuilds", &doc! {})
            .unwrap()
            .is_empty());
        // The build's row says which members are ready to commit.
        let update: Document = bson::from_slice(&rows[2].1).unwrap();
        assert!(update
            .get_document("o")
            .unwrap()
            .get_document("diff")
            .unwrap()
            .get_document("i")
            .unwrap()
            .contains_key("commitReadyMembers"));
        // The same entries read as the one `createIndexes` they were.
        let commit: Document = bson::from_slice(&rows[3].1).unwrap();
        let unwound = secantus_storage::oplog_entry::unwind(&commit);
        assert_eq!(
            unwound[0]
                .get_document("o")
                .unwrap()
                .get_str("createIndexes"),
            Ok("c")
        );
    });
}

/// A `createIndexes` of two indexes over documents is ONE build: one
/// `config.system.indexBuilds` row, one `startIndexBuild` and one
/// `commitIndexBuild`, each listing both specs (measured, mongod 8.2.11).
#[test]
fn two_indexes_over_documents_are_one_build() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1, "b": 2}))
            .unwrap();
        let before = st.oplog_tail_seq();
        let uuid = [9u8; 16];
        let spec = |name: &str, key: Document| secantus_storage::IndexBuildSpec {
            name: name.to_string(),
            key,
            options: Document::new(),
        };
        st.begin_index_build(
            "app",
            "c",
            uuid,
            &[spec("a_1", doc! {"a": 1}), spec("b_-1", doc! {"b": -1})],
            "localhost:27017",
        )
        .unwrap();
        st.run_index_build(&uuid, 64 << 20).unwrap();
        st.index_build_ready(&uuid, "localhost:27017").unwrap();
        st.commit_index_build(&uuid).unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        // `config.system.indexBuilds` was created with the store.
        assert_eq!(
            ops_of(&rows)
                .iter()
                .map(|o| o.1.as_str())
                .collect::<Vec<_>>(),
            vec![
                "config.system.indexBuilds _id",
                "app.$cmd startIndexBuild",
                "config.system.indexBuilds $v",
                "app.$cmd commitIndexBuild",
                "config.system.indexBuilds _id",
            ]
        );
        let row: Document = bson::from_slice(&rows[0].1).unwrap();
        let names: Vec<&str> = row
            .get_document("o")
            .unwrap()
            .get_array("indexNames")
            .unwrap()
            .iter()
            .filter_map(Bson::as_str)
            .collect();
        assert_eq!(names, ["a_1", "b_-1"]);
        for at in [1, 3] {
            let e: Document = bson::from_slice(&rows[at].1).unwrap();
            assert_eq!(
                e.get_document("o")
                    .unwrap()
                    .get_array("indexes")
                    .unwrap()
                    .len(),
                2
            );
        }
    });
}

/// An index on an empty collection is one `createIndexes` entry (measured).
#[test]
fn an_index_on_an_empty_collection_is_one_entry() {
    with_db(|st| {
        st.create_collection("app", "e").unwrap();
        let before = st.oplog_tail_seq();
        st.create_index("app", "e", "a_1", &doc! {"a": 1}, &Document::new())
            .unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        assert_eq!(
            ops_of(&rows),
            vec![("c".to_string(), "app.$cmd createIndexes".to_string())]
        );
        let e: Document = bson::from_slice(&rows[0].1).unwrap();
        let o = e.get_document("o").unwrap();
        assert_eq!(
            o.keys().collect::<Vec<_>>(),
            ["createIndexes", "v", "key", "name"]
        );
    });
}

/// A collection a transaction creates is logged as a member of its `applyOps`,
/// beside the insert (measured 8.2.11, the fixture's transaction steps).
#[test]
fn a_collection_a_transaction_creates_is_a_member_of_its_apply_ops() {
    with_db(|st| {
        let before = st.oplog_tail_seq();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_collection("app", "t1").unwrap();
            st.insert_one("app", "t1", &enc(&doc! {"_id": 1})).unwrap();
        })
        .unwrap();
        st.commit_user_transaction(&mut txn).unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        assert_eq!(rows.len(), 1, "{:?}", ops_of(&rows));
        let e: Document = bson::from_slice(&rows[0].1).unwrap();
        let members: Vec<(String, String)> = e
            .get_document("o")
            .unwrap()
            .get_array("applyOps")
            .unwrap()
            .iter()
            .map(|m| {
                let m = m.as_document().unwrap();
                (
                    m.get_str("op").unwrap().into(),
                    m.get_str("ns").unwrap().into(),
                )
            })
            .collect();
        assert_eq!(
            members,
            vec![
                ("c".to_string(), "app.$cmd".to_string()),
                ("i".to_string(), "app.t1".to_string())
            ]
        );
    });
}

/// A database or collection name that is not UTF-8 is held escaped inside the
/// server and logged as the bytes it stands for, as mongod logs it (RM-222);
/// the engine's readers get it escaped again.
#[test]
fn a_name_that_is_not_utf8_is_logged_as_its_bytes() {
    with_db(|st| {
        let name = secantus_core::bsondoc::escape_key(b"c\xff");
        let before = st.oplog_tail_seq();
        st.insert_one("u8", &name, &enc(&doc! {"_id": 1})).unwrap();
        let rows = st.read_oplog(before + 1, 100).unwrap();
        let blob = &rows.last().unwrap().1;
        assert!(
            blob.windows(5).any(|w| w == b"u8.c\xff"),
            "the entry names the collection by its bytes"
        );
        assert!(!blob.contains(&0u8) || !blob.windows(2).any(|w| w == b"c\0"));
        // The engine reads it escaped.
        let entry = secantus_core::bsondoc::from_bytes(blob).unwrap();
        let op = &secantus_storage::oplog_entry::unwind(&entry)[0];
        assert_eq!(op.get_str("ns").unwrap(), format!("u8.{name}"));
    });
}
