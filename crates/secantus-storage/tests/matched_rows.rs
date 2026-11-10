//! A write told which rows its caller's read already matched
//! (`with_matched_rows`, RM-391) does not test its filter on them again, as
//! mongod tests a write's filter once per document; a row another writer
//! changed since is tested. Against real WiredTiger.
//!
//! The filter here matches nothing, so a row written proves it was not
//! tested, and a row left alone proves it was.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-matched-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Three documents, and the first row as it is stored.
fn seeded(home: &std::path::Path) -> (Storage, (i64, Vec<u8>)) {
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let docs: Vec<Vec<u8>> = (0..3)
        .map(|i| bson::to_vec(&doc! {"_id": i, "n": 0}).unwrap())
        .collect();
    st.insert("app", "c", docs, true).unwrap();
    let rows = st
        .records_after("app", "c", None, usize::MAX, false)
        .unwrap();
    (st, rows[0].clone())
}

/// A filter no document matches.
fn none() -> Document {
    doc! {"n": 99}
}

fn update(st: &Storage, multi: bool) -> usize {
    st.update_matching(
        "app",
        "c",
        &none(),
        &doc! {"$inc": {"n": 1}},
        multi,
        false,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
    .unwrap()
    .modified
}

fn delete(st: &Storage, limit: usize) -> usize {
    st.delete_matching("app", "c", &none(), limit, &Document::new(), None)
        .unwrap()
}

type Write = fn(&Storage) -> usize;

const WRITES: [(&str, Write); 4] = [
    ("update one", |st| update(st, false)),
    ("update many", |st| update(st, true)),
    ("delete one", |st| delete(st, 1)),
    ("delete many", |st| delete(st, 0)),
];

#[test]
fn a_write_does_not_test_a_row_its_read_matched() {
    for (name, write) in WRITES {
        let home = temp_home();
        let (st, row) = seeded(&home);
        let n = secantus_storage::with_matched_rows(vec![row], || write(&st));
        assert_eq!(n, 1, "{name}: the matched row is written, untested");
        drop(st);
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn a_write_tests_a_row_changed_since_its_read_matched_it() {
    for (name, write) in WRITES {
        let home = temp_home();
        let (st, (recordid, _)) = seeded(&home);
        // The bytes the read matched are not the row's now.
        let then = bson::to_vec(&doc! {"_id": 0, "n": 99}).unwrap();
        let n = secantus_storage::with_matched_rows(vec![(recordid, then)], || write(&st));
        assert_eq!(n, 0, "{name}: a changed row is tested again");
        drop(st);
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn the_matched_rows_end_with_the_write() {
    let home = temp_home();
    let (st, row) = seeded(&home);
    secantus_storage::with_matched_rows(vec![row], || ());
    assert_eq!(
        update(&st, false),
        0,
        "outside the scope every row is tested"
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
