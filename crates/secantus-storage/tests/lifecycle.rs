//! Collection / database lifecycle tests (Phase 4 sub-phase 5b):
//! `create_collection`, `drop_collection`, `drop_database`,
//! `rename_collection`, and `list_databases` in the Rust storage engine —
//! data + index upkeep and the `op: "c"` command oplog entries. Against real
//! WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-life-{}-{}", std::process::id(), n));
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

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

/// All command (`op: "c"`) oplog entries emitted since `floor`, as decoded docs.
fn cmd_entries(st: &Storage, floor: i64) -> Vec<Document> {
    st.read_oplog(floor + 1, 1000)
        .unwrap()
        .into_iter()
        .map(|(_seq, blob)| decode(&blob))
        .filter(|e| e.get_str("op").ok() == Some("c"))
        .collect()
}

#[test]
fn create_collection_idempotent() {
    with_db(|st| {
        assert!(st.create_collection("app", "c").unwrap());
        assert!(st.collection_exists("app", "c").unwrap());
        // Second create is a no-op.
        assert!(!st.create_collection("app", "c").unwrap());
    });
}

#[test]
fn create_collection_emits_create_oplog() {
    with_db(|st| {
        let floor = st.oplog_tail_seq();
        st.create_collection("app", "c").unwrap();
        let cmds = cmd_entries(st, floor);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].get_str("ns").unwrap(), "app.$cmd");
        let o = cmds[0].get_document("o").unwrap();
        assert_eq!(o.get_str("create").unwrap(), "c");
        assert_eq!(
            o.get_document("idIndex").unwrap().get_str("name").unwrap(),
            "_id_"
        );
        assert!(matches!(cmds[0].get("ui"), Some(Bson::Binary(_))));
    });
}

#[test]
fn drop_collection_removes_data_and_indexes() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 5}))
            .unwrap();
        assert!(st.drop_collection("app", "c").unwrap());
        assert!(!st.collection_exists("app", "c").unwrap());
        // Data, index registry, and index entries are all gone.
        assert_eq!(st.scan_collection("app", "c").unwrap().len(), 0);
        assert_eq!(st.list_indexes("app", "c").unwrap().len(), 0);
        assert_eq!(st.index_entries("app", "c", "x_1").unwrap().len(), 0);
        // Dropping a missing collection returns false.
        assert!(!st.drop_collection("app", "c").unwrap());
    });
}

#[test]
fn drop_collection_emits_drop_oplog() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        let floor = st.oplog_tail_seq();
        st.drop_collection("app", "c").unwrap();
        let cmds = cmd_entries(st, floor);
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].get_document("o").unwrap().get_str("drop").unwrap(),
            "c"
        );
    });
}

#[test]
fn drop_database_removes_all_collections() {
    with_db(|st| {
        st.insert_one("app", "a", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("app", "b", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("other", "c", &enc(&doc! {"_id": 1})).unwrap();
        let floor = st.oplog_tail_seq();
        st.drop_database("app").unwrap();
        assert_eq!(st.list_collections("app").unwrap().len(), 0);
        // The other database is untouched.
        assert_eq!(st.scan_collection("other", "c").unwrap().len(), 1);
        // One `drop` per collection + a final `dropDatabase`.
        let cmds = cmd_entries(st, floor);
        let drops = cmds
            .iter()
            .filter(|e| e.get_document("o").unwrap().contains_key("drop"))
            .count();
        assert_eq!(drops, 2);
        assert!(cmds
            .iter()
            .any(|e| e.get_document("o").unwrap().contains_key("dropDatabase")));
    });
}

#[test]
fn rename_collection_moves_data_and_indexes() {
    with_db(|st| {
        st.create_index("app", "src", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "src", &enc(&doc! {"_id": 1, "x": 7}))
            .unwrap();
        st.insert_one("app", "src", &enc(&doc! {"_id": 2, "x": 8}))
            .unwrap();
        let (ok, msg) = st
            .rename_collection("app", "src", "app", "dst", false)
            .unwrap();
        assert!(ok);
        assert!(msg.is_none());
        // Source is gone; destination has the data and a working index.
        assert!(!st.collection_exists("app", "src").unwrap());
        assert_eq!(st.scan_collection("app", "dst").unwrap().len(), 2);
        assert_eq!(
            st.find_matching("app", "dst", &doc! {"x": 7})
                .unwrap()
                .len(),
            1
        );
    });
}

#[test]
fn rename_missing_source_fails() {
    with_db(|st| {
        let (ok, msg) = st
            .rename_collection("app", "nope", "app", "dst", false)
            .unwrap();
        assert!(!ok);
        assert!(msg.unwrap().contains("source namespace does not exist"));
    });
}

#[test]
fn rename_target_exists_requires_drop_target() {
    with_db(|st| {
        st.insert_one("app", "src", &enc(&doc! {"_id": 1, "v": "src"}))
            .unwrap();
        st.insert_one("app", "dst", &enc(&doc! {"_id": 9, "v": "dst"}))
            .unwrap();
        // Without drop_target: refused.
        let (ok, msg) = st
            .rename_collection("app", "src", "app", "dst", false)
            .unwrap();
        assert!(!ok);
        assert!(msg.unwrap().contains("target namespace exists"));
        // With drop_target: the old dst is replaced by src's data.
        let (ok, _) = st
            .rename_collection("app", "src", "app", "dst", true)
            .unwrap();
        assert!(ok);
        let docs = st.scan_collection("app", "dst").unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(decode(&docs[0]).get_str("v").unwrap(), "src");
    });
}

#[test]
fn rename_same_namespace_is_noop() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        let (ok, msg) = st.rename_collection("app", "c", "app", "c", false).unwrap();
        assert!(ok);
        assert!(msg.is_none());
        assert_eq!(st.scan_collection("app", "c").unwrap().len(), 1);
    });
}

#[test]
fn list_databases_includes_local_with_oplog() {
    with_db(|st| {
        st.insert_one("alpha", "c", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("beta", "c", &enc(&doc! {"_id": 1})).unwrap();
        let dbs = st.list_databases().unwrap();
        assert!(dbs.contains(&"alpha".to_string()));
        assert!(dbs.contains(&"beta".to_string()));
        // Oplog is on by default — mongod always exposes `local`.
        assert!(dbs.contains(&"local".to_string()));
        // Sorted.
        let mut sorted = dbs.clone();
        sorted.sort();
        assert_eq!(dbs, sorted);
    });
}
