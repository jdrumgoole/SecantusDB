//! Oplog foundation tests (Phase 4 sub-phase 3, slice 3a): insert emission
//! (op "i"), strictly-monotonic seq + timestamp minting, `read_oplog` /
//! `oplog_floor_seq` / `oplog_tail_seq`, `current_cluster_time`, and seq recovery
//! across a close/reopen. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, StorageOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-oplog-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Open the store with the collections these tests write already there.
/// A write that creates its collection records the creation in the oplog,
/// and these tests count entries from the first write.
fn open_quiet(home: &std::path::Path) -> Storage {
    let mut st = Storage::open(home.to_str().unwrap()).unwrap();
    st.set_enable_oplog(false);
    st.create_collection("app", "c").unwrap();
    st.create_collection("app", "other").unwrap();
    st.set_enable_oplog(true);
    st
}

fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = open_quiet(&home);
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// Where the entries these tests write begin: a store's oplog starts with the
/// set's initiating no-op and the creation of `config.system.indexBuilds`,
/// which mongod writes too.
fn after_init(st: &Storage) -> i64 {
    let rows = st.read_oplog(1, 2).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        decode(&rows[1].1)
            .get_document("o")
            .unwrap()
            .get_str("create"),
        Ok("system.indexBuilds")
    );
    rows[1].0 + 1
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

fn ts_of(blob: &[u8]) -> (u32, u32) {
    match decode(blob).get("ts") {
        Some(Bson::Timestamp(t)) => (t.time, t.increment),
        other => panic!("expected ts Timestamp, got {other:?}"),
    }
}

/// An entry's position in the oplog is its `ts`, `secs << 32 | inc`: the
/// RecordId of `local.oplog.rs` is derived from the timestamp, as mongod's is.
fn pos_of(blob: &[u8]) -> i64 {
    let (secs, inc) = ts_of(blob);
    (i64::from(secs) << 32) | i64::from(inc)
}

#[test]
fn insert_emits_oplog_insert_entry() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 7}))
            .unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        assert_eq!(rows.len(), 1);
        let (seq, blob) = &rows[0];
        assert_eq!(*seq, pos_of(blob), "the key of a row is its timestamp");
        let e = decode(blob);
        assert_eq!(e.get_str("op").unwrap(), "i");
        assert_eq!(e.get_str("ns").unwrap(), "app.c");
        assert_eq!(e.get_document("o").unwrap().get_i32("x").unwrap(), 7);
        assert_eq!(e.get_document("o2").unwrap().get_i32("_id").unwrap(), 1);
        assert!(matches!(e.get("ts"), Some(Bson::Timestamp(_))));
        assert!(matches!(e.get("wall"), Some(Bson::DateTime(_))));
    });
}

#[test]
fn seqs_and_timestamps_monotonic() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        assert_eq!(rows.len(), 3);
        for (seq, blob) in &rows {
            assert_eq!(*seq, pos_of(blob), "RecordId = ts on every row");
        }
        for w in rows.windows(2) {
            assert!(
                ts_of(&w[1].1) > ts_of(&w[0].1),
                "timestamps must strictly increase"
            );
        }
        assert_eq!(st.oplog_tail_seq(), rows[2].0);
        assert_eq!(
            st.oplog_floor_seq().unwrap(),
            st.read_oplog(1, 1).unwrap()[0].0
        );
    });
}

#[test]
fn read_oplog_honours_start_and_limit() {
    with_db(|st| {
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        st.flush_oplog();
        let all = st.read_oplog(after_init(st), 100).unwrap();
        assert_eq!(all.len(), 5);
        let from3 = st.read_oplog(all[2].0, 100).unwrap();
        assert_eq!(
            from3.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            all[2..].iter().map(|(s, _)| *s).collect::<Vec<_>>()
        );
        // A start between two entries reads from the next one.
        let between = st.read_oplog(all[1].0 + 1, 100).unwrap();
        assert_eq!(between[0].0, all[2].0);
        st.flush_oplog();
        let limited = st.read_oplog(after_init(st), 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].0, all[0].0);
    });
}

#[test]
fn current_cluster_time_is_monotonic() {
    with_db(|st| {
        let a = st.current_cluster_time().unwrap();
        let b = st.current_cluster_time().unwrap();
        assert!((b.time, b.increment) > (a.time, a.increment));
    });
}

#[test]
fn empty_oplog_reads_are_empty() {
    with_db(|st| {
        st.flush_oplog();
        // Only the initiating no-op.
        assert!(st.read_oplog(after_init(st), 10).unwrap().is_empty());
        assert_eq!(
            st.oplog_floor_seq().unwrap(),
            st.read_oplog(1, 1).unwrap()[0].0
        );
        assert_eq!(st.oplog_tail_seq(), after_init(st) - 1);
    });
}

#[test]
fn reopen_recovers_seq_counter() {
    let home = temp_home();
    let tail = {
        let st = open_quiet(&home);
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        let tail = st.oplog_tail_seq();
        assert_eq!(tail, st.read_oplog(after_init(&st), 10).unwrap()[2].0);
        tail
    };
    {
        // The clock resumes from the oplog's newest entry (as mongod reads
        // the top of its oplog) and continues past it.
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        assert_eq!(st.oplog_tail_seq(), tail);
        st.insert_one("app", "c", &enc(&doc! {"_id": 4})).unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(tail + 1, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].0 > tail,
            "continues past the last entry, no collision"
        );
        assert_eq!(rows[0].0, pos_of(&rows[0].1));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn replace_emits_update_entry_with_full_doc() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 1}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"x": 2}))
            .unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        assert_eq!(rows.len(), 2);
        // seq 1 = insert, seq 2 = the replacement.
        let u = decode(&rows[1].1);
        assert_eq!(u.get_str("op").unwrap(), "u");
        assert_eq!(u.get_str("ns").unwrap(), "app.c");
        // Replacement logs the FULL new doc in `o` (not a $v:2 diff).
        let o = u.get_document("o").unwrap();
        assert_eq!(o.get_i32("x").unwrap(), 2);
        assert_eq!(o.get_i32("_id").unwrap(), 1);
        assert_eq!(u.get_document("o2").unwrap().get_i32("_id").unwrap(), 1);
    });
}

#[test]
fn delete_emits_delete_entry() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 7})).unwrap();
        assert!(st.delete_by_id("app", "c", &Bson::Int32(7)).unwrap());
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        assert_eq!(rows.len(), 2);
        let d = decode(&rows[1].1);
        assert_eq!(d.get_str("op").unwrap(), "d");
        assert_eq!(d.get_str("ns").unwrap(), "app.c");
        assert_eq!(d.get_document("o").unwrap().get_i32("_id").unwrap(), 7);
        // mongod's delete entry has no `o2`.
        assert!(!d.contains_key("o2"));
    });
}

#[test]
fn insert_replace_delete_sequence() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "n": 0}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"n": 1}))
            .unwrap();
        st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap();
        st.flush_oplog();
        let ops: Vec<String> = st
            .read_oplog(after_init(st), 100)
            .unwrap()
            .iter()
            .map(|(_, b)| decode(b).get_str("op").unwrap().to_string())
            .collect();
        assert_eq!(ops, vec!["i", "u", "d"]);
    });
}

/// The `ui` (collection UUID) bytes from an entry — asserting Binary subtype 4.
fn ui_of(blob: &[u8]) -> Vec<u8> {
    match decode(blob).get("ui") {
        Some(Bson::Binary(b)) => {
            assert_eq!(b.subtype, bson::spec::BinarySubtype::Uuid);
            assert_eq!(b.bytes.len(), 16);
            b.bytes.clone()
        }
        other => panic!("expected ui Binary subtype 4, got {other:?}"),
    }
}

fn seq_of_op(st: &Storage, op: &str) -> i64 {
    st.flush_oplog();
    st.read_oplog(after_init(st), 100)
        .unwrap()
        .into_iter()
        .find(|(_, b)| decode(b).get_str("op").unwrap() == op)
        .unwrap_or_else(|| panic!("no oplog entry with op {op}"))
        .0
}

#[test]
fn entries_carry_stable_collection_uuid() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2})).unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        let u1 = ui_of(&rows[0].1);
        // Same collection -> same ui, matching the public accessor.
        assert_eq!(u1, ui_of(&rows[1].1));
        assert_eq!(u1, st.collection_uuid("app", "c").unwrap());
        // A different collection gets a distinct ui.
        st.insert_one("app", "other", &enc(&doc! {"_id": 1}))
            .unwrap();
        st.flush_oplog();
        let other = st.read_oplog(rows[1].0 + 1, 1).unwrap();
        assert_ne!(ui_of(&other[0].1), u1);
    });
}

#[test]
fn pre_images_written_when_enabled() {
    with_db(|st| {
        st.set_collection_options(
            "app",
            "c",
            &doc! {"changeStreamPreAndPostImages": {"enabled": true}},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "old"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "new"}))
            .unwrap();
        // The replacement's pre-image is the doc as it was before.
        let pre = st
            .read_preimage(seq_of_op(st, "u"))
            .unwrap()
            .expect("update pre-image present");
        assert_eq!(decode(&pre).get_str("v").unwrap(), "old");
        // The delete's pre-image is the doc at delete time.
        st.delete_by_id("app", "c", &Bson::Int32(1)).unwrap();
        let pred = st
            .read_preimage(seq_of_op(st, "d"))
            .unwrap()
            .expect("delete pre-image present");
        assert_eq!(decode(&pred).get_str("v").unwrap(), "new");
        // Inserts never carry a pre-image.
        assert!(st.read_preimage(seq_of_op(st, "i")).unwrap().is_none());
    });
}

#[test]
fn no_pre_images_when_disabled() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "b"}))
            .unwrap();
        assert!(st.read_preimage(seq_of_op(st, "u")).unwrap().is_none());
    });
}

fn with_db_mut(body: impl FnOnce(&mut Storage)) {
    let home = temp_home();
    let mut st = open_quiet(&home);
    body(&mut st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn prune_oplog_by_retention() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        let tail = st.oplog_tail_seq();
        // The initiating no-op begins every oplog.
        assert!(st.oplog_reaches_genesis().unwrap());
        let held = st.read_oplog(1, 100).unwrap().len();
        // `now` far in the future -> every entry is past the retention window.
        assert_eq!(st.prune_oplog(Some(10_000_000_000)).unwrap(), held);
        st.flush_oplog();
        assert!(st.read_oplog(1, 10).unwrap().is_empty());
        assert_eq!(st.oplog_floor_seq().unwrap(), 0);
        assert_eq!(st.oplog_tail_seq(), tail); // the tail is unaffected
                                               // The oplog no longer begins at the initiating no-op.
        assert!(!st.oplog_reaches_genesis().unwrap());
    });
}

#[test]
fn prune_oplog_keeps_recent() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        // `now`=0 -> negative cutoff -> nothing is old enough to drop.
        let held = st.read_oplog(1, 100).unwrap().len();
        assert_eq!(st.prune_oplog(Some(0)).unwrap(), 0);
        st.flush_oplog();
        assert_eq!(st.read_oplog(1, 100).unwrap().len(), held);
        assert!(st.oplog_reaches_genesis().unwrap());
    });
}

#[test]
fn prune_oplog_size_cap() {
    with_db_mut(|st| {
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        let rows = st.read_oplog(1, 100).unwrap();
        let before: Vec<i64> = rows.iter().map(|(s, _)| *s).collect();
        assert!(before.len() >= 6, "{before:?}");
        // Size the oplog to hold its last two entries, whole: the oldest
        // entries go, however many that is, and the oplog stays under it.
        let keep = 2;
        let size: usize = rows[rows.len() - keep..].iter().map(|(_, b)| b.len()).sum();
        st.set_oplog_max_bytes(size as i64).unwrap();
        // Retention drops nothing (now=0); the size takes the older ones out.
        //
        // `set_oplog_max_bytes` wakes the background pruner (mongod's
        // OplogCapMaintainerThread), which applies the same size at once, so
        // it can take some or all of the doomed entries before this sweep
        // runs. Sweeps are exclusive and each entry goes exactly once, so
        // this call's count is only a bound; what the size demands is the
        // oplog held afterwards, asserted exactly below.
        let dropped = st.prune_oplog(Some(0)).unwrap();
        assert!(dropped <= before.len() - keep, "{dropped} of {before:?}");
        st.flush_oplog();
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(
            rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            before[before.len() - keep..].to_vec()
        );
        assert_eq!(st.oplog_floor_seq().unwrap(), before[before.len() - keep]);
        assert!(!st.oplog_reaches_genesis().unwrap());
        // The held bytes are the size: a further sweep removes nothing.
        assert_eq!(st.prune_oplog(Some(0)).unwrap(), 0);
    });
}

/// The flake `prune_oplog_size_cap` once was: `set_oplog_max_bytes` wakes the
/// background pruner, which holds the oplog to the new size with no call from
/// the caller. A caller whose own sweep runs after that finds nothing left to
/// drop, so the size is enforced either way -- this pins the background half.
#[test]
fn oplog_size_cap_is_applied_by_the_background_pruner() {
    with_db_mut(|st| {
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        let rows = st.read_oplog(1, 100).unwrap();
        let before: Vec<i64> = rows.iter().map(|(s, _)| *s).collect();
        let keep = 2;
        let size: usize = rows[rows.len() - keep..].iter().map(|(_, b)| b.len()).sum();
        st.set_oplog_max_bytes(size as i64).unwrap();
        let want = before[before.len() - keep..].to_vec();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            st.flush_oplog();
            let held: Vec<i64> = st
                .read_oplog(1, 100)
                .unwrap()
                .iter()
                .map(|(s, _)| *s)
                .collect();
            if held == want {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the background pruner left {held:?}, wanted {want:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Nothing is left for an explicit sweep: it is the same cap.
        assert_eq!(st.prune_oplog(Some(0)).unwrap(), 0);
    });
}

#[test]
fn prune_removes_paired_pre_images() {
    with_db(|st| {
        st.set_collection_options(
            "app",
            "c",
            &doc! {"changeStreamPreAndPostImages": {"enabled": true}},
        )
        .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": "a"}))
            .unwrap();
        st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"v": "b"}))
            .unwrap();
        let upd_seq = seq_of_op(st, "u");
        assert!(st.read_preimage(upd_seq).unwrap().is_some());
        st.prune_oplog(Some(10_000_000_000)).unwrap();
        assert!(st.read_preimage(upd_seq).unwrap().is_none()); // pre-image gone too
    });
}

#[test]
fn noop_heartbeat_entry() {
    with_db(|st| {
        let seq = st.emit_noop_heartbeat().unwrap();
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 10).unwrap();
        assert_eq!(seq, rows[0].0);
        assert_eq!(seq, pos_of(&rows[0].1));
        let e = decode(&rows[0].1);
        assert_eq!(e.get_str("op").unwrap(), "n");
        assert_eq!(e.get_str("ns").unwrap(), "");
        assert_eq!(
            e.get_document("o").unwrap().get_str("msg").unwrap(),
            "periodic noop"
        );
        assert_eq!(st.oplog_tail_seq(), seq);
    });
}

#[test]
fn find_seq_for_ts_locates_entry() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        st.flush_oplog();
        let rows = st.read_oplog(after_init(st), 100).unwrap();
        let ts2 = match decode(&rows[1].1).get("ts") {
            Some(Bson::Timestamp(t)) => *t,
            _ => panic!("no ts"),
        };
        assert_eq!(st.find_seq_for_ts(ts2).unwrap(), rows[1].0);
        // A timestamp past the tail resolves to next_seq (= tail + 1).
        let beyond = bson::Timestamp {
            time: u32::MAX,
            increment: u32::MAX,
        };
        assert_eq!(st.find_seq_for_ts(beyond).unwrap(), st.oplog_tail_seq() + 1);
    });
}

#[test]
fn reopen_mints_past_the_newest_entry() {
    // Nothing is persisted for the oplog's counters: the clock resumes from
    // the table's newest row, so a new entry can never take a position an
    // existing one holds (re-minting one would overwrite a live row).
    let home = temp_home();
    let first = {
        let st = open_quiet(&home);
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        st.read_oplog(after_init(&st), 10)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect::<Vec<_>>()
    };
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 4})).unwrap();
        st.flush_oplog();
        let rows: Vec<i64> = st
            .read_oplog(after_init(&st), 10)
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[..3], first[..]);
        assert!(rows[3] > first[2]);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn cluster_time_monotonic_across_reopen() {
    let home = temp_home();
    let before = {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        let mut last = st.current_cluster_time().unwrap();
        for _ in 0..3 {
            last = st.current_cluster_time().unwrap();
        }
        last
    };
    // Cluster-time mints are never persisted per call; recovery instead bumps
    // the clock one full second past everything it can see, so post-reopen
    // mints are strictly greater even when the reopen happens within the same
    // wall second as the last pre-close mint.
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let after = st.current_cluster_time().unwrap();
    assert!((after.time, after.increment) > (before.time, before.increment));
    // Close WT before removing its data dir (see concurrent_writes.rs). This
    // reopened handle does no writes, so it doesn't panic today — but the
    // remove-before-drop ordering is the same latent bug, so keep it correct.
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// How long the operation counted in `scope` held oplog slots open.
fn slot_micros(scope: &secantus_storage::OperationScope) -> i64 {
    scope.counts().oplog_slot_micros
}

/// `with_db`, with the oplog on and the collections the tests write made
/// first. The oplog slot is timed from mint to commit (RM-361).
fn with_each_oplog(body: impl Fn(&Storage)) {
    let home = temp_home();
    let mut st =
        Storage::open_with_options(home.to_str().unwrap(), &StorageOptions::default()).unwrap();
    st.set_enable_oplog(false);
    st.create_collection("app", "c").unwrap();
    st.create_collection("app", "other").unwrap();
    st.set_enable_oplog(true);
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_write_counts_how_long_it_held_its_oplog_slots() {
    // mongod's `totalOplogSlotDurationMicros` (RM-361): each slot is timed
    // from its reservation to the commit of the write that took it.
    with_each_oplog(|st| {
        let docs: Vec<Vec<u8>> = (0..200).map(|i| enc(&doc! {"_id": i, "x": i})).collect();
        let scope = secantus_storage::operation_scope();
        st.insert("app", "c", docs, true).unwrap();
        assert!(slot_micros(&scope) > 0);
    });
}

#[test]
fn a_write_that_rolls_back_or_mints_nothing_counts_no_slot() {
    with_each_oplog(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1})).unwrap();
        // A duplicate key rolls its write back: mongod counts nothing.
        let scope = secantus_storage::operation_scope();
        assert!(st.insert_one("app", "c", &enc(&doc! {"_id": 1})).is_err());
        assert_eq!(slot_micros(&scope), 0);
        drop(scope);
        // A read takes no slot.
        let scope = secantus_storage::operation_scope();
        st.scan_collection("app", "c").unwrap();
        assert_eq!(slot_micros(&scope), 0);
    });
}

#[test]
fn a_transactions_slots_are_its_commits() {
    // mongod reserves a transaction's slots when it commits, so its
    // statements count none and `commitTransaction` counts them.
    with_each_oplog(|st| {
        let mut txn = st.begin_user_transaction().unwrap();
        let statement = secantus_storage::operation_scope();
        st.with_user_transaction(&mut txn, || {
            for i in 0..50 {
                st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
            }
        })
        .unwrap();
        assert_eq!(slot_micros(&statement), 0);
        drop(statement);
        let commit = secantus_storage::operation_scope();
        st.commit_user_transaction(&mut txn).unwrap();
        assert!(slot_micros(&commit) > 0);
    });
}

#[test]
fn an_operation_inside_another_adds_its_slots_to_it() {
    with_each_oplog(|st| {
        let outer = secantus_storage::operation_scope();
        {
            let _inner = secantus_storage::operation_scope();
            let docs: Vec<Vec<u8>> = (0..200).map(|i| enc(&doc! {"_id": i})).collect();
            st.insert("app", "c", docs, true).unwrap();
        }
        assert!(slot_micros(&outer) > 0);
    });
}

#[test]
fn reopening_a_timestamped_store_again_and_again_never_commits_behind_wiredtiger() {
    // A write with no oplog entry of its own (a catalog write at open) commits
    // at a timestamp, and the next checkpoint's stable timestamp covers it. The
    // clock cannot resume from the newest entry alone: a commit older than the
    // stable timestamp is refused (EINVAL). Reopen within one wall second,
    // several times, each time with a write.
    let home = temp_home();
    let opts = secantus_storage::StorageOptions {
        durable: Some(true),
        data_nonlogged: Some(true),
        // Unlogged data is recovered from a logged oplog (RM-731), whatever
        // the non-logged-oplog CI lane asks.
        oplog_nonlogged: Some(false),
        ..secantus_storage::StorageOptions::default()
    };
    let mut last_tail = 0;
    for i in 0..6 {
        let st = Storage::open_with_options(home.to_str().unwrap(), &opts).unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        let tail = st.oplog_tail_seq();
        assert!(tail > last_tail, "the oplog's tail moves on at every open");
        last_tail = tail;
    }
    let _ = std::fs::remove_dir_all(&home);
}
