//! The time-series bucket catalog over real WiredTiger: where measurements
//! go (one at a time, as mongod 8.2.11 placed the same measurements), what
//! a restart forgets, and the conversion of a store whose measurements
//! predate stored buckets (RM-088).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bson::{doc, Bson, Document};
use secantus_commands::{dispatch, CommandContext, CursorRegistry, Storage as CmdStorage};
use secantus_storage::Storage as WtStorage;
use secantus_storage_adapter::StorageAdapter;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh store, and a way to open a server over it (a new adapter is a
/// restart: its bucket catalog starts empty).
fn with_store(body: impl FnOnce(&Arc<WtStorage>)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-tscat-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
    body(&wt);
    drop(wt);
    let _ = std::fs::remove_dir_all(&dir);
}

fn server(wt: &Arc<WtStorage>) -> (Arc<dyn CmdStorage>, CommandContext) {
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt.clone()));
    let mut ctx = CommandContext::new(1)
        .with_storage(adapter.clone())
        .with_cursors(Arc::new(CursorRegistry::new()));
    ctx.db_name = "t".into();
    (adapter, ctx)
}

fn at(minutes: f64) -> Bson {
    // 2026-01-01T00:00:00Z
    Bson::DateTime(bson::DateTime::from_millis(
        1_767_225_600_000 + (minutes * 60_000.0) as i64,
    ))
}

fn ok(reply: &Document) {
    assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
    assert!(!reply.contains_key("writeErrors"), "{reply}");
}

fn create(c: &mut CommandContext) {
    ok(&dispatch(
        &doc! {"create": "w", "timeseries": {"timeField": "t", "metaField": "m"}},
        c,
    ));
}

fn insert_one(c: &mut CommandContext, d: Document) {
    ok(&dispatch(&doc! {"insert": "w", "documents": [d]}, c));
}

fn first_batch(reply: &Document) -> Vec<Document> {
    let cursor = reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("no cursor: {reply}"));
    let batch = match cursor.get_array("firstBatch") {
        Ok(b) => b.clone(),
        Err(_) => Vec::new(),
    };
    batch
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

/// The `_id`s a scan returns, and each stored bucket's count.
fn placed(c: &mut CommandContext) -> (Vec<i32>, Vec<i32>) {
    let found = common_find(c, "w");
    let ids = found.iter().map(|d| d.get_i32("_id").unwrap()).collect();
    let counts = common_find(c, "system.buckets.w")
        .iter()
        .map(|b| b.get_document("control").unwrap().get_i32("count").unwrap())
        .collect();
    (ids, counts)
}

fn common_find(c: &mut CommandContext, coll: &str) -> Vec<Document> {
    let reply = dispatch(&doc! {"find": coll, "batchSize": 10_000}, c);
    if let Some(batch) = c.pending_batch.take() {
        return batch
            .batch
            .iter()
            .map(|raw| bson::from_slice(raw).unwrap())
            .collect();
    }
    first_batch(&reply)
}

/// mongod 8.2.11 made two buckets of `{a, b}` meta (keys in either order
/// are one series), reopened the 00:01 bucket for 00:30, and started
/// another for 02:00 (measured one measurement at a time, 2026-11-09).
#[test]
fn measurements_fall_into_mongods_buckets() {
    with_store(|wt| {
        let (_, mut c) = server(wt);
        create(&mut c);
        for d in [
            doc! {"_id": 1, "t": at(0.0), "m": {"b": 1, "a": 2}, "v": 1, "s": "x"},
            doc! {"_id": 2, "t": at(1.0), "m": "b", "v": 2},
            doc! {"_id": 3, "t": at(2.0), "m": {"a": 2, "b": 1}, "v": 3.5},
            doc! {"_id": 4, "t": at(3.0), "v": 4},
            doc! {"_id": 5, "t": at(120.0), "m": "b", "v": 5},
            doc! {"_id": 6, "t": at(30.0), "m": "b", "v": 6},
        ] {
            insert_one(&mut c, d);
        }
        assert_eq!(placed(&mut c), (vec![1, 3, 2, 6, 4, 5], vec![2, 2, 1, 1]));
    });
}

/// mongod 8.2.11: a field that changes kind rolls the bucket over, a
/// number of another type does not, and a measurement running backwards
/// reopens the EARLIEST bucket whose window holds it (the reopening query
/// reads the meta and time index in order).
#[test]
fn a_schema_change_rolls_the_bucket_over() {
    with_store(|wt| {
        let (_, mut c) = server(wt);
        create(&mut c);
        for d in [
            doc! {"_id": 0, "t": at(30.0), "m": "a", "v": 1},
            doc! {"_id": 1, "t": at(31.0), "m": "a", "v": "s"},
            doc! {"_id": 2, "t": at(32.0), "m": "a", "v": 3},
            doc! {"_id": 3, "t": at(33.0), "m": "a", "v": 4.5},
            doc! {"_id": 4, "t": at(34.0), "m": "a", "v": 5_i64, "w": {"x": 1}},
            doc! {"_id": 5, "t": at(35.0), "m": "a", "w": {"x": 2, "y": [1]}},
            doc! {"_id": 6, "t": at(36.0), "m": "a", "w": {"x": "no"}},
            doc! {"_id": 7, "t": at(31.5), "m": "a"},
        ] {
            insert_one(&mut c, d);
        }
        assert_eq!(
            placed(&mut c),
            (vec![0, 7, 1, 2, 3, 4, 5, 6], vec![2, 1, 4, 1])
        );
    });
}

/// A restart forgets the catalog: the next measurement for the series is
/// placed by the reopening query, into the stored bucket, which it makes
/// unsorted. What mongod 8.2.11 answered for the same writes across a
/// restart (2026-11-11): one bucket of five, and these counters.
#[test]
fn a_restart_reopens_the_stored_bucket() {
    with_store(|wt| {
        {
            let (_, mut c) = server(wt);
            create(&mut c);
            ok(&dispatch(
                &doc! {"insert": "w", "documents": [
                    {"_id": 0, "t": at(0.0), "m": 1},
                    {"_id": 1, "t": at(1.0), "m": 1},
                    {"_id": 2, "t": at(2.0), "m": 1},
                ]},
                &mut c,
            ));
        }
        let (_, mut c) = server(wt);
        insert_one(&mut c, doc! {"_id": 8, "t": at(1.5), "m": 1});
        insert_one(&mut c, doc! {"_id": 9, "t": at(10.0), "m": 1});
        let buckets = common_find(&mut c, "system.buckets.w");
        assert_eq!(buckets.len(), 1);
        let control = buckets[0].get_document("control").unwrap();
        assert_eq!(control.get_i32("count").ok(), Some(5));
        assert_eq!(control.get_i32("version").ok(), Some(3));
        let stats = dispatch(&doc! {"collStats": "w"}, &mut c);
        let ts = stats.get_document("timeseries").unwrap();
        let moved: Vec<(String, i32)> = ts
            .iter()
            .filter(|(k, _)| k.starts_with("num"))
            .filter_map(|(k, v)| v.as_i32().filter(|n| *n != 0).map(|n| (k.clone(), n)))
            .collect();
        let want: Vec<(String, i32)> = [
            ("numActiveBuckets", 1),
            ("numBucketUpdates", 2),
            ("numCommits", 2),
            ("numMeasurementsCommitted", 2),
            ("numBucketsReopened", 1),
            ("numCompressedBucketsConvertedToUnsorted", 1),
            ("numBucketsQueried", 1),
        ]
        .into_iter()
        .map(|(k, n)| (k.to_string(), n))
        .collect();
        assert_eq!(moved, want);
    });
}

// Retired at storage-model phase 7 (store format 9): `an_old_store_is_converted_on_open`
// built a pre-phase-5 store by inserting measurements into a time-series
// collection's own table. A time-series collection is now mongod's view
// over its buckets, with no table, so no store this build opens can hold
// one; stores of formats 4 to 9 are refused by name at open
// (`store_format.rs`). The conversion path (`upgrade_time_series`) went
// in phase 8, leaving only `create_missing_buckets`.
