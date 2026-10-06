//! `Storage::block_overlay` and its first consumer, `scan_matching_batches`
//! under `with_read_overlay`: a READ COMMITTED block's own rows laid over a
//! fresh snapshot, so a read sees other sessions' later commits AND the
//! block's uncommitted writes without replaying its write set. Real
//! WiredTiger; the sync and async oplog lanes both run it (async answers
//! `None`, the caller's cue to replay instead).

use bson::{doc, Bson, Document};
use secantus_storage::{with_read_overlay, Storage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("secantus-overlay-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all::<PathBuf>(home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn scan(st: &Storage, filter: &Document) -> Vec<Document> {
    let mut out = Vec::new();
    st.scan_matching_batches("app", "t", filter, 2, |b| {
        out.extend(b.iter().map(|x| bson::from_slice::<Document>(x).unwrap()));
        true
    })
    .unwrap();
    out
}

#[test]
fn overlay_merges_the_blocks_rows_over_a_fresh_snapshot() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "t", &enc(&doc! {"_id": i, "v": i * 10}))
                .unwrap();
        }
        st.insert_one("app", "other", &enc(&doc! {"_id": 1}))
            .unwrap();
        let mut block = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut block, || -> secantus_storage::Result<()> {
            st.replace_by_id("app", "t", &Bson::Int32(2), &enc(&doc! {"_id": 2, "v": 21}))?;
            st.delete_by_id("app", "t", &Bson::Int32(3))?;
            st.insert_one("app", "t", &enc(&doc! {"_id": 4, "v": 40}))?;
            st.delete_by_id("app", "other", &Bson::Int32(1))?;
            Ok(())
        })
        .unwrap()
        .unwrap();
        // Another session commits after the block's snapshot.
        st.replace_by_id("app", "t", &Bson::Int32(1), &enc(&doc! {"_id": 1, "v": 11}))
            .unwrap();
        st.insert_one("app", "t", &enc(&doc! {"_id": 5, "v": 50}))
            .unwrap();

        let ov = st.block_overlay(&mut block, "app", "t").unwrap();
        if st.oplog_async() {
            assert!(ov.is_none(), "async oplog has no readable write set rows");
            st.rollback_user_transaction(&mut block).unwrap();
            return;
        }
        let ov = ov.expect("a row-only write set gives an overlay");
        assert_eq!(ov.rows.len(), 3);
        assert_eq!(ov.rows.values().filter(|r| r.is_none()).count(), 1);

        // Without the overlay: the committed state only.
        let plain = scan(st, &doc! {});
        assert_eq!(
            plain,
            vec![
                doc! {"_id": 1, "v": 11},
                doc! {"_id": 2, "v": 20},
                doc! {"_id": 3, "v": 30},
                doc! {"_id": 5, "v": 50},
            ]
        );
        let mut m = HashMap::new();
        m.insert(("app".to_string(), "t".to_string()), ov);
        let m = Arc::new(m);
        let laid = with_read_overlay(Arc::clone(&m), || scan(st, &doc! {}));
        // RecordId order: 4 was inserted by the block before 5 committed.
        assert_eq!(
            laid,
            vec![
                doc! {"_id": 1, "v": 11},
                doc! {"_id": 2, "v": 21},
                doc! {"_id": 4, "v": 40},
                doc! {"_id": 5, "v": 50},
            ]
        );
        // The filter applies to the block's rows too, and the stale version
        // of an overlaid row never matches in its place.
        let laid = with_read_overlay(Arc::clone(&m), || scan(st, &doc! {"v": {"$lt": 21}}));
        assert_eq!(laid, vec![doc! {"_id": 1, "v": 11}]);
        let laid = with_read_overlay(Arc::clone(&m), || scan(st, &doc! {"v": 20}));
        assert!(laid.is_empty());
        // Restored after the call.
        assert_eq!(scan(st, &doc! {}), plain);
        // A table the block did not write gets an empty overlay.
        let other = st
            .block_overlay(&mut block, "app", "none")
            .unwrap()
            .unwrap();
        assert!(other.rows.is_empty());
        st.rollback_user_transaction(&mut block).unwrap();
    });
}

#[test]
fn a_command_on_the_table_refuses_the_overlay() {
    with_db(|st| {
        st.insert_one("app", "t", &enc(&doc! {"_id": 1})).unwrap();
        let mut block = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut block, || st.drop_collection("app", "t"))
            .unwrap()
            .unwrap();
        assert_eq!(st.block_overlay(&mut block, "app", "t").unwrap(), None);
        st.rollback_user_transaction(&mut block).unwrap();
    });
}

fn decode_all(v: Vec<Vec<u8>>) -> Vec<Document> {
    v.iter().map(|b| bson::from_slice(b).unwrap()).collect()
}

/// Every read path a PostgreSQL READ COMMITTED statement can reach inside
/// `read_apart` -- a fresh user transaction with the block's overlay
/// installed: `find_matching_with` (an indexed filter, a sort), the count,
/// the `_id` point read and the resumable `scan_batch_after`.
#[test]
fn every_read_path_honours_the_overlay_inside_a_fresh_transaction() {
    with_db(|st| {
        st.create_index("app", "t", "v_1", &doc! {"v": 1}, &doc! {})
            .unwrap();
        for i in 1..=3 {
            st.insert_one("app", "t", &enc(&doc! {"_id": i, "v": i * 10}))
                .unwrap();
        }
        let mut block = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut block, || -> secantus_storage::Result<()> {
            st.replace_by_id("app", "t", &Bson::Int32(2), &enc(&doc! {"_id": 2, "v": 21}))?;
            st.delete_by_id("app", "t", &Bson::Int32(3))?;
            st.insert_one("app", "t", &enc(&doc! {"_id": 4, "v": 5}))?;
            Ok(())
        })
        .unwrap()
        .unwrap();
        st.insert_one("app", "t", &enc(&doc! {"_id": 5, "v": 50}))
            .unwrap();
        let Some(ov) = st.block_overlay(&mut block, "app", "t").unwrap() else {
            assert!(st.oplog_async());
            st.rollback_user_transaction(&mut block).unwrap();
            return;
        };
        let mut m = HashMap::new();
        m.insert(("app".to_string(), "t".to_string()), ov);
        let m = Arc::new(m);
        let mut tmp = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut tmp, || {
            with_read_overlay(Arc::clone(&m), || {
                // The index still holds v=20 for row 2 and v=30 for row 3.
                let f = |q: Document| {
                    decode_all(
                        st.find_matching_with("app", "t", &q, None, None, None, &doc! {})
                            .unwrap(),
                    )
                };
                assert!(f(doc! {"v": 20}).is_empty());
                assert!(f(doc! {"v": {"$gte": 30, "$lt": 40}}).is_empty());
                assert_eq!(f(doc! {"v": 21}), vec![doc! {"_id": 2, "v": 21}]);
                assert_eq!(f(doc! {"_id": 3}), Vec::<Document>::new());
                let sorted = decode_all(
                    st.find_matching_with(
                        "app",
                        "t",
                        &doc! {},
                        Some(&doc! {"v": -1}),
                        None,
                        None,
                        &doc! {},
                    )
                    .unwrap(),
                );
                let ids: Vec<i32> = sorted.iter().map(|d| d.get_i32("_id").unwrap()).collect();
                assert_eq!(ids, vec![5, 2, 1, 4]);
                assert_eq!(st.count_matching("app", "t", &doc! {}, None).unwrap(), 4);
                assert_eq!(
                    st.count_matching("app", "t", &doc! {"v": {"$lt": 25}}, None)
                        .unwrap(),
                    3
                );
                assert_eq!(st.find_by_id("app", "t", &Bson::Int32(3)).unwrap(), None);
                assert_eq!(
                    bson::from_slice::<Document>(
                        &st.find_by_id("app", "t", &Bson::Int32(2)).unwrap().unwrap()
                    )
                    .unwrap(),
                    doc! {"_id": 2, "v": 21}
                );
                // Paged one row at a time, the resume point crossing between
                // the snapshot's rows and the block's.
                let mut after = None;
                let mut paged = Vec::new();
                loop {
                    let (rows, next) = st.scan_batch_after("app", "t", &doc! {}, after, 1).unwrap();
                    paged.extend(decode_all(rows));
                    if next.is_none() {
                        break;
                    }
                    after = next;
                }
                let ids: Vec<i32> = paged.iter().map(|d| d.get_i32("_id").unwrap()).collect();
                assert_eq!(ids, vec![1, 2, 4, 5]);
            })
        })
        .unwrap();
        st.rollback_user_transaction(&mut tmp).unwrap();
        // Without the overlay the fresh snapshot is the committed state.
        let plain = decode_all(st.find_matching("app", "t", &doc! {"v": 20}).unwrap());
        assert_eq!(plain, vec![doc! {"_id": 2, "v": 20}]);
        st.rollback_user_transaction(&mut block).unwrap();
    });
}
