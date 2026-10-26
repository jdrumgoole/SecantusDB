//! Snapshot reads at a point in time over real WiredTiger (RM-210): a store
//! whose data tables are not logged -- the one `secantusd-rs` creates --
//! reads at `atClusterTime`, cursors and all; a logged one keeps no history
//! and refuses a time in the past by name rather than answer as of now.
//! The mongod comparisons are `tests/backlog/test_rm_found_2026_10_20b.py`.

// Shares the helpers; uses only some of them.
#[allow(dead_code)]
mod common;

use std::sync::Arc;

use bson::{doc, Bson, Document};
use common::dispatch_full;
use secantus_commands::cursors::CursorRegistry;
use secantus_commands::storage::Storage as CmdStorage;
use secantus_commands::CommandContext;
use secantus_storage::{Storage as WtStorage, StorageOptions};
use secantus_storage_adapter::StorageAdapter;

fn with_store(nonlogged: bool, body: impl FnOnce(&mut CommandContext)) {
    let dir = std::env::temp_dir().join(format!(
        "secantus-snapshot-wt-{}-{}",
        std::process::id(),
        nonlogged
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wt = Arc::new(
        WtStorage::open_with_options(
            dir.to_str().unwrap(),
            &StorageOptions {
                data_nonlogged: Some(nonlogged),
                transactions_logged_at_commit: true,
                ..StorageOptions::default()
            },
        )
        .unwrap(),
    );
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
    let mut ctx = CommandContext::new(1)
        .with_storage(adapter)
        .with_cursors(Arc::new(CursorRegistry::new()));
    ctx.db_name = "t".into();
    ctx.replica_set_name = Some("rs".into());
    body(&mut ctx);
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Five documents at `n: 0`, then the time, then all set to `n: 9`.
fn seed(c: &mut CommandContext) -> bson::Timestamp {
    let docs: Vec<Document> = (0..5).map(|i| doc! {"_id": i, "n": 0}).collect();
    dispatch_full(&doc! {"insert": "c", "documents": docs}, c);
    let t = c.storage.as_ref().unwrap().peek_cluster_time();
    dispatch_full(
        &doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"n": 9}}, "multi": true}]},
        c,
    );
    t
}

fn ns(batch: &Document, key: &str) -> Vec<i32> {
    batch
        .get_document("cursor")
        .unwrap()
        .get_array(key)
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().get_i32("n").unwrap())
        .collect()
}

#[test]
fn a_find_and_its_get_mores_read_at_the_time_asked() {
    with_store(true, |c| {
        let t = seed(c);
        let reply = dispatch_full(
            &doc! {"find": "c", "batchSize": 2,
            "readConcern": {"level": "snapshot", "atClusterTime": Bson::Timestamp(t)}},
            c,
        );
        assert_eq!(ns(&reply, "firstBatch"), vec![0, 0], "{reply}");
        let cursor = reply.get_document("cursor").unwrap();
        assert_eq!(cursor.get("atClusterTime"), Some(&Bson::Timestamp(t)));
        let id = cursor.get_i64("id").unwrap();
        dispatch_full(
            &doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"n": 7}}, "multi": true}]},
            c,
        );
        let more = dispatch_full(&doc! {"getMore": id, "collection": "c"}, c);
        assert_eq!(ns(&more, "nextBatch"), vec![0, 0, 0], "{more}");
    });
}

#[test]
fn a_logged_store_refuses_a_time_in_the_past() {
    with_store(false, |c| {
        let t = seed(c);
        let reply = dispatch_full(
            &doc! {"find": "c",
            "readConcern": {"level": "snapshot", "atClusterTime": Bson::Timestamp(t)}},
            c,
        );
        assert_eq!(reply.get_i32("code"), Ok(246), "{reply}");
        assert_eq!(reply.get_str("codeName"), Ok("SnapshotUnavailable"));
        // Without a time it reads the latest data, which is as of now.
        let now = dispatch_full(&doc! {"find": "c", "readConcern": {"level": "snapshot"}}, c);
        assert_eq!(ns(&now, "firstBatch"), vec![9, 9, 9, 9, 9], "{now}");
    });
}
