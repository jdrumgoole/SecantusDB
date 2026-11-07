//! Change stream pre-images are documents of `config.system.preimages`, as
//! mongod 8.2.11 keeps them (RM-366): `{_id: {nsUUID, ts, applyOpsIndex},
//! operationTime, preImage}`, written with the write, expired with the
//! oplog and by `changeStreamOptions`, and rebuilt by crash recovery.
//! Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageOptions, PRE_IMAGES_COLL, PRE_IMAGES_DB};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-preimages-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

fn open(home: &Path, options: &StorageOptions) -> Storage {
    let mut st = Storage::open_with_options(home.to_str().unwrap(), options).unwrap();
    st.set_enable_oplog(true);
    st
}

/// `app.c`, recording pre-images.
fn recording(st: &Storage) {
    st.create_collection("app", "c").unwrap();
    st.set_collection_options(
        "app",
        "c",
        &doc! {"changeStreamPreAndPostImages": {"enabled": true}},
    )
    .unwrap();
}

fn with_store(options: StorageOptions, body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = open(&home, &options);
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn pre_images(st: &Storage) -> Vec<Document> {
    st.scan_collection(PRE_IMAGES_DB, PRE_IMAGES_COLL)
        .unwrap()
        .iter()
        .map(|b| decode(b))
        .collect()
}

/// Every stored oplog entry of `op` on `app.c`, with its seq.
fn entries_of(st: &Storage, op: &str) -> Vec<(i64, Document)> {
    st.flush_oplog();
    st.read_oplog(1, 1000)
        .unwrap()
        .into_iter()
        .map(|(seq, b)| (seq, decode(&b)))
        .filter(|(_, e)| e.get_str("op") == Ok(op) && e.get_str("ns") == Ok("app.c"))
        .collect()
}

fn check_one_update_pre_image(st: &Storage) {
    recording(st);
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "old"}))
        .unwrap();
    st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "new"}))
        .unwrap();
    let (seq, entry) = entries_of(st, "u").pop().expect("the update's entry");
    let found = pre_images(st);
    assert_eq!(found.len(), 1, "{found:?}");
    let record = &found[0];
    assert_eq!(
        record.keys().collect::<Vec<_>>(),
        ["_id", "operationTime", "preImage"]
    );
    let expected_id = doc! {
        "nsUUID": entry.get("ui").unwrap().clone(),
        "ts": entry.get("ts").unwrap().clone(),
        "applyOpsIndex": 0_i64,
    };
    assert_eq!(record.get_document("_id").unwrap(), &expected_id);
    assert_eq!(record.get("operationTime"), entry.get("wall"));
    assert_eq!(
        record.get_document("preImage").unwrap(),
        &doc! {"_id": 1, "v": "old"}
    );
    // Read back for a change stream by the entry's own `_id`.
    let pre = st.read_preimage(seq).unwrap().expect("found by its entry");
    assert_eq!(decode(&pre), doc! {"_id": 1, "v": "old"});
    // The collection is clustered, in mongod's legacy form.
    assert_eq!(
        st.get_collection_options(PRE_IMAGES_DB, PRE_IMAGES_COLL)
            .unwrap()
            .get("clusteredIndex"),
        Some(&Bson::Boolean(true))
    );
}

/// A pre-image is a `config.system.preimages` document in mongod's shape,
/// under the `_id` its oplog entry names, and read back from there.
#[test]
fn a_pre_image_is_a_document_of_config_system_preimages() {
    with_store(StorageOptions::default(), check_one_update_pre_image);
}

/// The same through the asynchronous oplog, where the writer records the
/// pre-image after its write commits and before the drainer writes the
/// entry.
#[test]
fn a_pre_image_is_recorded_on_the_asynchronous_oplog() {
    with_store(
        StorageOptions {
            oplog_async: Some(true),
            data_nonlogged: Some(false),
            ..StorageOptions::default()
        },
        check_one_update_pre_image,
    );
}

/// The deletes of one batch are one `applyOps` entry in the oplog view, so
/// their pre-images share its time and number their places in it -- and
/// each is still found from its own stored entry.
#[test]
fn a_batch_s_pre_images_share_its_time() {
    with_store(StorageOptions::default(), |st| {
        recording(st);
        let docs: Vec<Vec<u8>> = (1..=3).map(|i| enc(&doc! {"_id": i, "v": i})).collect();
        st.insert("app", "c", docs, true).unwrap();
        secantus_storage::with_reversed_delete_batches(10, || {
            st.delete_matching("app", "c", &Document::new(), 0, &Document::new(), None)
                .unwrap();
        });
        let deletes = entries_of(st, "d");
        assert_eq!(deletes.len(), 3);
        let first_ts = deletes[0].1.get_timestamp("ts").unwrap();
        let mut found = pre_images(st);
        found.sort_by_key(|d| {
            d.get_document("_id")
                .unwrap()
                .get_i64("applyOpsIndex")
                .unwrap()
        });
        assert_eq!(found.len(), 3, "{found:?}");
        for (i, (record, (seq, entry))) in found.iter().zip(&deletes).enumerate() {
            let id = record.get_document("_id").unwrap();
            assert_eq!(id.get_timestamp("ts").unwrap(), first_ts);
            assert_eq!(id.get_i64("applyOpsIndex").unwrap(), i as i64);
            let deleted = entry.get_document("o").unwrap().get("_id").unwrap().clone();
            assert_eq!(
                record.get_document("preImage").unwrap().get("_id"),
                Some(&deleted)
            );
            let pre = decode(&st.read_preimage(*seq).unwrap().expect("found"));
            assert_eq!(pre.get("_id"), Some(&deleted));
        }
    });
}

/// A pre-image goes when the oplog entry of its write is pruned, as
/// mongod's remover expires one older than the oplog, and not before.
#[test]
fn a_pre_image_expires_with_its_oplog_entry() {
    with_store(StorageOptions::default(), |st| {
        recording(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "b"}))
            .unwrap();
        let (seq, _) = entries_of(st, "u").pop().unwrap();
        // Nothing is old enough to go.
        st.prune_oplog(Some(0)).unwrap();
        assert_eq!(pre_images(st).len(), 1);
        st.prune_oplog(Some(10_000_000_000)).unwrap();
        assert!(pre_images(st).is_empty());
        assert!(st.read_preimage(seq).unwrap().is_none());
    });
}

/// `changeStreamOptions.preAndPostImages.expireAfterSeconds` expires a
/// pre-image recorded longer ago than that, though its entry is kept.
#[test]
fn a_pre_image_expires_after_the_seconds_set() {
    with_store(StorageOptions::default(), |st| {
        recording(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "b"}))
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Off (mongod's default): kept.
        st.prune_oplog(Some(now + 100)).unwrap();
        assert_eq!(pre_images(st).len(), 1);
        st.set_pre_image_expire_after_seconds(Some(1000));
        st.prune_oplog(Some(now + 100)).unwrap();
        assert_eq!(pre_images(st).len(), 1);
        st.set_pre_image_expire_after_seconds(Some(10));
        st.prune_oplog(Some(now + 100)).unwrap();
        assert!(pre_images(st).is_empty());
        // The oplog keeps its entry (inside its hour's retention).
        assert_eq!(entries_of(st, "u").len(), 1);
    });
}

/// A store whose data tables are not logged (how `secantusd-rs` creates
/// one) loses the pre-images written after its last stable checkpoint in a
/// crash, with the rest of that data; crash recovery's replay of the oplog
/// records them again, from each document as it was before the write, as
/// mongod's oplog application does.
#[test]
fn crash_recovery_records_the_pre_images_it_replays() {
    const CHILD: &str = "SECANTUS_PREIMAGES_CRASH_CHILD";
    // `secantusd-rs`'s store: unlogged data tables over a logged oplog,
    // whatever the environment selects (`SECANTUS_OPLOG_NONLOGGED` would
    // make the oplog as forgetful as the data, leaving nothing to replay).
    let options = StorageOptions {
        durable: Some(true),
        data_nonlogged: Some(true),
        oplog_nonlogged: Some(false),
        oplog_async: Some(false),
        ..StorageOptions::default()
    };
    if let Ok(home) = std::env::var(CHILD) {
        // The child: write, then end the process without closing the store.
        let st = open(Path::new(&home), &options);
        recording(&st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "old"}))
            .unwrap();
        st.checkpoint_now().unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "new"}))
            .unwrap();
        st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap();
        st.flush_oplog();
        // The journal on disk, as the flusher puts it: the oplog with it,
        // the unlogged data tables as the checkpoint left them.
        st.flush_journal().unwrap();
        std::process::exit(0);
    }
    let home = temp_home();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_recovery_records_the_pre_images_it_replays",
            "--test-threads=1",
        ])
        .env(CHILD, home.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    let st = open(&home, &options);
    let (useq, _) = entries_of(&st, "u").pop().expect("the update survived");
    let (dseq, _) = entries_of(&st, "d").pop().expect("the delete survived");
    let pre = st
        .read_preimage(useq)
        .unwrap()
        .expect("the update's pre-image");
    assert_eq!(decode(&pre), doc! {"_id": 1, "v": "old"});
    let pre = st
        .read_preimage(dseq)
        .unwrap()
        .expect("the delete's pre-image");
    assert_eq!(decode(&pre), doc! {"_id": 1, "v": "new"});
    assert_eq!(pre_images(&st).len(), 2);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
