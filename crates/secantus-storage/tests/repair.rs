//! `secantusd-rs --repair` (RM-192): every table verified and a damaged one
//! salvaged, then the indexes of the collections it touched rebuilt from
//! their documents, as mongod's `--repair` does.

use bson::{doc, spec::BinarySubtype, Binary, Bson, Document};
use secantus_storage::repair::repair;
use secantus_storage::{Storage, StorageOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-repair-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn options(server_like: bool) -> StorageOptions {
    StorageOptions {
        wt_config: Some(secantus_storage::wt_config("256M", 1000, false, "10MB")),
        data_nonlogged: Some(server_like),
        durable: Some(true),
        transactions_logged_at_commit: server_like,
        ..Default::default()
    }
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn document(i: i64) -> Document {
    doc! {
        "_id": i,
        "k": i % 97,
        "p": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: noise(i as u64, 1000) }),
    }
}

/// A store with `app.c` (5,000 documents, an index on `k`), closed.
fn seeded(home: &Path, server_like: bool) {
    let st = Storage::open_with_options(home.to_str().unwrap(), &options(server_like)).unwrap();
    st.create_index("app", "c", "k_1", &doc! {"k": 1}, &doc! {})
        .unwrap();
    for start in (0..5_000).step_by(500) {
        let docs: Vec<Vec<u8>> = (start..start + 500)
            .map(|i| bson::to_vec(&document(i)).unwrap())
            .collect();
        st.insert("app", "c", docs, true).unwrap();
    }
    st.close().unwrap();
}

/// Overwrite 16 KB in the middle of a table's file.
fn damage(home: &Path, table: &str) {
    let path = home.join(format!("{}.wt", table.trim_start_matches("table:")));
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 64 * 1024, "{table} is only {len} bytes");
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(len / 2)).unwrap();
    f.write_all(&[0xde; 16 * 1024]).unwrap();
    f.sync_all().unwrap();
}

/// The collection's indexes agree with its documents; answers how many
/// documents it has.
fn consistent(st: &Storage, db: &str, coll: &str) -> i64 {
    let check = st.check_collection(db, coll).unwrap();
    assert_eq!(check.get_i64("undecodable").unwrap(), 0, "{check:?}");
    for ix in check.get_array("indexes").unwrap() {
        let ix = ix.as_document().unwrap();
        assert_eq!(ix.get_i64("missing").unwrap_or(0), 0, "{check:?}");
        assert_eq!(ix.get_i64("extra").unwrap_or(0), 0, "{check:?}");
    }
    check.get_i64("records").unwrap()
}

#[test]
fn repair_of_a_healthy_store_changes_nothing() {
    let home = temp_home();
    seeded(&home, false);
    let mut lines = Vec::new();
    let report = repair(home.to_str().unwrap(), &options(false), &mut |l| {
        lines.push(l.to_string())
    })
    .unwrap();
    assert!(report.salvaged.is_empty(), "{report:?}");
    assert!(report.rebuilt.is_empty(), "{report:?}");
    assert!(report
        .verified
        .contains(&secantus_storage::collection_table("app", "c")));
    assert!(
        lines.iter().any(|l| l == "Repairing collection app.c"),
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .any(|l| l == "Validate did not make any repairs: app.c"));
    let st = Storage::open_with_options(home.to_str().unwrap(), &options(false)).unwrap();
    assert_eq!(consistent(&st, "app", "c"), 5_000);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn salvages_a_damaged_document_shard(server_like: bool) {
    let home = temp_home();
    seeded(&home, server_like);
    let shard = secantus_storage::collection_table("app", "c");
    damage(&home, &shard);
    let mut lines = Vec::new();
    let report = repair(home.to_str().unwrap(), &options(server_like), &mut |l| {
        lines.push(l.to_string())
    })
    .unwrap();
    assert_eq!(report.salvaged, vec![shard.clone()], "{lines:?}");
    assert_eq!(report.rebuilt, vec!["app.c".to_string()]);
    assert!(lines
        .iter()
        .any(|l| l.starts_with("Verify failed") && l.ends_with(&shard)));

    let st = Storage::open_with_options(home.to_str().unwrap(), &options(server_like)).unwrap();
    let kept = consistent(&st, "app", "c");
    assert!(kept > 4_000 && kept < 5_000, "kept {kept}");
    // Every document that survived reads back whole, by `_id` and by `k`.
    let mut by_id = 0;
    for i in 0..5_000i64 {
        if let Some(blob) = st.find_by_id("app", "c", &Bson::Int64(i)).unwrap() {
            assert_eq!(blob, bson::to_vec(&document(i)).unwrap());
            by_id += 1;
        }
    }
    assert_eq!(by_id, kept);
    // The repaired store takes writes again.
    st.insert(
        "app",
        "c",
        vec![bson::to_vec(&document(9_999)).unwrap()],
        true,
    )
    .unwrap();
    assert_eq!(consistent(&st, "app", "c"), kept + 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn repair_salvages_a_damaged_document_shard_and_rebuilds_its_indexes() {
    salvages_a_damaged_document_shard(false);
}

#[test]
fn repair_salvages_a_store_opened_as_the_server_opens_it() {
    salvages_a_damaged_document_shard(true);
}

#[test]
fn repair_rebuilds_the_indexes_from_a_damaged_index_table() {
    let home = temp_home();
    seeded(&home, false);
    damage(&home, "table:secantus_index_entries");
    let report = repair(home.to_str().unwrap(), &options(false), &mut |_| {}).unwrap();
    assert_eq!(
        report.salvaged,
        vec!["table:secantus_index_entries".to_string()]
    );
    assert_eq!(report.rebuilt, vec!["app.c".to_string()]);
    let st = Storage::open_with_options(home.to_str().unwrap(), &options(false)).unwrap();
    // No document was in the damaged table: all of them are there, and the
    // index has every entry again.
    assert_eq!(consistent(&st, "app", "c"), 5_000);
    assert_eq!(st.index_entries("app", "c", "k_1").unwrap().len(), 5_000);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn repair_of_a_missing_directory_is_an_error() {
    let home = temp_home().join("absent");
    let err = repair(home.to_str().unwrap(), &options(false), &mut |_| {}).unwrap_err();
    assert!(format!("{err}").contains("not found"), "{err}");
}
