//! The `_id` point-lookup fast paths must stay OFF for a timeseries
//! collection. Its rows carry a suffixed `id_key` (duplicates are legal), so
//! the fast path -- which reconstructs the UNsuffixed key -- would find
//! nothing. `is_timeseries` answers from the raw options blob now (it was 40%
//! of a one-row-by-key `find_matching`); these pin that the answer did not
//! change for the three shapes of options blob a collection can have.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-ts-{}-{}", std::process::id(), n));
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

fn dec(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

fn ids(rows: &[Vec<u8>]) -> Vec<i32> {
    rows.iter()
        .map(|r| dec(r).get_i32("_id").unwrap())
        .collect()
}

/// Mirrors `tests/test_storage.py::test_timeseries_allows_duplicate_ids`:
/// two rows share `_id: 1`, and a `{_id: 1}` filter sees BOTH.
#[test]
fn timeseries_id_lookup_sees_every_duplicate() {
    with_db(|st| {
        st.create_collection("ts", "m").unwrap();
        st.set_collection_options("ts", "m", &doc! {"timeseries": {"timeField": "time"}})
            .unwrap();
        let t0 = bson::DateTime::from_millis(1_552_949_630_000);
        st.insert_one("ts", "m", &enc(&doc! {"_id": 1, "time": t0}))
            .unwrap();
        st.insert_one("ts", "m", &enc(&doc! {"_id": 1, "time": t0}))
            .unwrap();
        assert_eq!(ids(&st.find_matching("ts", "m", &doc! {}).unwrap()), [1, 1]);
        let by_id = st.find_matching("ts", "m", &doc! {"_id": 1}).unwrap();
        assert_eq!(ids(&by_id), [1, 1], "the _id fast path must not engage");
        assert_eq!(
            st.count_matching("ts", "m", &doc! {"_id": 1}, None)
                .unwrap(),
            2
        );
        let removed = st
            .delete_matching("ts", "m", &doc! {"_id": 1}, 0, &doc! {}, None)
            .unwrap();
        assert_eq!(removed, 2);
        assert!(st.find_matching("ts", "m", &doc! {}).unwrap().is_empty());
    });
}

/// A collection whose options blob is EMPTY (created bare) and one whose
/// options carry other keys but no `timeseries` both keep the fast path --
/// and the fast path answers the same rows a scan does.
#[test]
fn non_timeseries_id_lookup_is_unchanged() {
    with_db(|st| {
        st.create_collection("app", "bare").unwrap();
        st.create_collection_with_options("app", "opts", &doc! {"capped": false})
            .unwrap();
        for coll in ["bare", "opts"] {
            for i in 1..=3 {
                st.insert_one("app", coll, &enc(&doc! {"_id": i, "v": i * 10}))
                    .unwrap();
            }
            let by_id = st.find_matching("app", coll, &doc! {"_id": 2}).unwrap();
            assert_eq!(by_id.len(), 1, "{coll}");
            assert_eq!(dec(&by_id[0]).get_i32("v").unwrap(), 20, "{coll}");
            // A second insert of the same `_id` IS a duplicate here.
            assert!(st.insert_one("app", coll, &enc(&doc! {"_id": 2})).is_err());
        }
        // A collection that never existed: no options row at all.
        assert!(st
            .find_matching("app", "missing", &doc! {"_id": 1})
            .unwrap()
            .is_empty());
    });
}
