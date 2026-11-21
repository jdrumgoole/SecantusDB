//! The oplog held to its size in bytes, and the state a new store is
//! initiated with (stages 4b-5 and 4b-6 of `tasks/mongod-storage-model-plan.md`).
//! The shapes are mongod 8.2.11's, measured by
//! `tools/probes/mongod_replset_state.py`
//! (`tests/fixtures/storage_model/mongod-8.2.11-replset-state.json`).

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-oplogsize-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    bson::from_slice(b).unwrap()
}

fn held(st: &Storage) -> (usize, usize) {
    let rows = st.read_oplog(1, 1_000_000).unwrap();
    (rows.len(), rows.iter().map(|(_, b)| b.len()).sum())
}

fn only(st: &Storage, coll: &str) -> Document {
    let docs = st.find_matching("local", coll, &doc! {}).unwrap();
    assert_eq!(docs.len(), 1, "local.{coll} holds one document");
    decode(&docs[0])
}

#[test]
fn a_new_store_is_initiated_as_mongod_initiates_a_set() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        // The oplog begins with the initiating no-op: `op, ns, o, ts, v,
        // wall`, and no `t`.
        let rows = st.read_oplog(1, 100).unwrap();
        let first = decode(&rows[0].1);
        assert_eq!(
            first.keys().collect::<Vec<_>>(),
            ["op", "ns", "o", "ts", "v", "wall"]
        );
        assert_eq!(first.get_str("op"), Ok("n"));
        assert_eq!(first.get_str("ns"), Ok(""));
        assert_eq!(
            first.get_document("o").unwrap(),
            &doc! {"msg": "initiating set"}
        );
        assert!(st.oplog_reaches_genesis().unwrap());

        // What mongod keeps in `local` after its initiation, field for field.
        let minvalid = only(&st, "replset.minvalid");
        assert_eq!(minvalid.keys().collect::<Vec<_>>(), ["_id", "t", "ts"]);
        assert!(matches!(minvalid.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(minvalid.get("t"), Some(&Bson::Int64(-1)));
        assert_eq!(
            minvalid.get("ts"),
            Some(&Bson::Timestamp(bson::Timestamp {
                time: 0,
                increment: 0
            }))
        );
        let point = only(&st, "replset.oplogTruncateAfterPoint");
        assert_eq!(
            point.keys().collect::<Vec<_>>(),
            ["_id", "oplogTruncateAfterPoint"]
        );
        assert_eq!(point.get_str("_id"), Ok("oplogTruncateAfterPoint"));
        assert!(matches!(
            point.get("oplogTruncateAfterPoint"),
            Some(Bson::Timestamp(_))
        ));
        let election = only(&st, "replset.election");
        assert_eq!(
            election.keys().collect::<Vec<_>>(),
            ["_id", "term", "candidateIndex"]
        );
        assert_eq!(election.get("term"), Some(&Bson::Int64(1)));
        assert_eq!(election.get("candidateIndex"), Some(&Bson::Int64(0)));
        let sync = only(&st, "replset.initialSyncId");
        assert_eq!(sync.keys().collect::<Vec<_>>(), ["_id", "wallTime"]);
        match sync.get("_id") {
            Some(Bson::Binary(b)) => {
                assert_eq!(b.subtype, bson::spec::BinarySubtype::Uuid);
                assert_eq!(b.bytes.len(), 16);
            }
            other => panic!("initialSyncId _id: {other:?}"),
        }
        assert!(matches!(sync.get("wallTime"), Some(Bson::DateTime(_))));
        assert_eq!(
            only(&st, "system.rollback.id"),
            doc! {"_id": "rollbackId", "rollbackId": 1_i32}
        );
        assert!(st
            .collection_exists("config", "system.indexBuilds")
            .unwrap());
        // None of it is in the oplog but the creation of the build's
        // collection, which mongod logs.
        let ns: Vec<String> = rows[1..]
            .iter()
            .map(|(_, b)| decode(b).get_str("ns").unwrap().to_string())
            .collect();
        assert_eq!(ns, ["config.$cmd"]);
    }
    // A clean reopen adds nothing.
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        assert_eq!(st.read_oplog(1, 100).unwrap().len(), 2);
        assert_eq!(
            only(&st, "system.rollback.id"),
            doc! {"_id": "rollbackId", "rollbackId": 1_i32}
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_oplog_is_held_to_its_size_in_whole_entries() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let max: i64 = 30_000;
    st.set_oplog_max_bytes(max).unwrap();
    for i in 0..200 {
        st.insert_one("app", "c", &enc(&doc! {"_id": i, "p": "x".repeat(500)}))
            .unwrap();
    }
    let (entries, bytes) = held(&st);
    assert!(bytes as i64 > max, "{bytes} bytes written past {max}");
    // The kept totals are what the table holds.
    assert_eq!(
        st.collection_data_size("local", "oplog.rs").unwrap(),
        bytes as i64
    );
    let pruned = st.prune_oplog(Some(0)).unwrap();
    assert!(pruned > 0);
    let (after, after_bytes) = held(&st);
    assert_eq!(after, entries - pruned);
    assert!(
        after_bytes as i64 <= max,
        "{after_bytes} bytes held of {max}"
    );
    // Whole entries from the oldest end: the newest survives, and one more
    // would not fit.
    let rows = st.read_oplog(1, 1_000_000).unwrap();
    let one_more = rows[0].1.len() as i64 + after_bytes as i64;
    assert!(one_more > max || after == entries);
    assert_eq!(
        st.collection_data_size("local", "oplog.rs").unwrap(),
        after_bytes as i64
    );
    assert!(!st.oplog_reaches_genesis().unwrap());
    // The data is untouched.
    assert_eq!(st.find_matching("app", "c", &doc! {}).unwrap().len(), 200);
    drop(st);
    // The size, and the totals, outlive the process.
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    assert_eq!(st.oplog_max_bytes(), max);
    assert_eq!(
        st.collection_data_size("local", "oplog.rs").unwrap(),
        held(&st).1 as i64
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_rolled_back_transaction_adds_nothing_to_the_size() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 0})).unwrap();
    let before = st.collection_data_size("local", "oplog.rs").unwrap();
    let mut txn = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "p": "y".repeat(2000)}))
    })
    .unwrap()
    .unwrap();
    st.rollback_user_transaction(&mut txn).unwrap();
    assert_eq!(
        st.collection_data_size("local", "oplog.rs").unwrap(),
        before
    );
    let mut txn = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "p": "z".repeat(2000)}))
    })
    .unwrap()
    .unwrap();
    st.commit_user_transaction(&mut txn).unwrap();
    let after = st.collection_data_size("local", "oplog.rs").unwrap();
    assert!(after > before + 2000);
    assert_eq!(after, held(&st).1 as i64);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_resize_is_the_catalog_entrys_size_and_survives_a_reopen() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        st.set_oplog_max_bytes(1_049_100_288).unwrap();
        let options = st.get_collection_options("local", "oplog.rs").unwrap();
        assert_eq!(options.get_i64("size"), Ok(1_049_100_288));
        let entry = st.mdb_catalog_entry("local", "oplog.rs").unwrap().unwrap();
        assert_eq!(
            entry
                .get_document("md")
                .unwrap()
                .get_document("options")
                .unwrap()
                .get("size")
                .and_then(Bson::as_i64),
            Some(1_049_100_288)
        );
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    assert_eq!(st.oplog_max_bytes(), 1_049_100_288);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_new_store_takes_the_size_it_is_asked_for() {
    let home = temp_home();
    let options = StorageOptions {
        oplog_size_bytes: Some(5 * 1024 * 1024),
        ..StorageOptions::default()
    };
    {
        let st = Storage::open_with_options(home.to_str().unwrap(), &options).unwrap();
        assert_eq!(st.oplog_max_bytes(), 5 * 1024 * 1024);
    }
    // An existing store keeps its own, whatever is asked now.
    let other = StorageOptions {
        oplog_size_bytes: Some(9 * 1024 * 1024),
        ..StorageOptions::default()
    };
    let st = Storage::open_with_options(home.to_str().unwrap(), &other).unwrap();
    assert_eq!(st.oplog_max_bytes(), 5 * 1024 * 1024);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_default_size_is_mongods_for_the_platform() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let size = st.oplog_max_bytes();
    if cfg!(target_os = "macos") {
        // Measured: mongod 8.2.11's `maxSize` on a Mac.
        assert_eq!(size, 201_326_592);
    } else {
        assert!((990 << 20..=50 << 30).contains(&size), "{size}");
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// mongod 8.2.11 (measured, `mongod-8.2.11-replset-state.json`): `rollbackId`
/// is 1 at initiation, 2 after the first kill -9 and 3 after the second, and a
/// clean restart leaves it. The count needs the lock file's word that the last
/// run did not shut down cleanly, so the servers' option is used.
#[test]
fn rollback_id_counts_each_unclean_shutdown_and_no_clean_one() {
    const CHILD: &str = "SECANTUS_ROLLBACK_ID_CHILD";
    let options = StorageOptions {
        lock_file: Some("mongod.lock".to_string()),
        ..StorageOptions::default()
    };
    if let Ok(home) = std::env::var(CHILD) {
        let st = Storage::open_with_options(&home, &options).unwrap();
        // The journal on disk, as its flusher puts it within 100 ms.
        st.flush_journal().unwrap();
        // No close: the lock file stays full, as after a kill -9.
        std::process::exit(0);
    }
    let rollback = |st: &Storage| {
        only(st, "system.rollback.id")
            .get_i32("rollbackId")
            .unwrap()
    };
    let home = temp_home();
    let h = home.to_str().unwrap();
    drop(Storage::open_with_options(h, &options).unwrap());
    for _ in 0..2 {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rollback_id_counts_each_unclean_shutdown_and_no_clean_one",
                "--test-threads=1",
            ])
            .env(CHILD, h)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let st = Storage::open_with_options(h, &options).unwrap();
    assert_eq!(rollback(&st), 3);
    drop(st);
    // A clean close and a reopen leave it.
    let st = Storage::open_with_options(h, &options).unwrap();
    assert_eq!(rollback(&st), 3);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
