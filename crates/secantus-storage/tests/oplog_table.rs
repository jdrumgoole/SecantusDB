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
/// `_mdb_catalog` is not logged, so the catalog row that names the oplog's table
/// is gone with the crash: the store must find the table by what it records in
/// the (logged) oplog meta table, or it makes a new empty oplog, drops the old
/// as an orphan, and every acknowledged write is lost.
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
    // The catalog names it again, with the uuid the store recorded.
    let entry = st.mdb_catalog_entry("local", "oplog.rs").unwrap().unwrap();
    assert_eq!(
        format!("table:{}", entry.get_str("ident").unwrap()),
        st.oplog_table()
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(format!("{}.uri", home.to_str().unwrap()));
}
