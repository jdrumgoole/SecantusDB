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
