//! Oplog foundation tests (Phase 4 sub-phase 3, slice 3a): insert emission
//! (op "i"), strictly-monotonic seq + timestamp minting, `read_oplog` /
//! `oplog_floor_seq` / `oplog_tail_seq`, `current_cluster_time`, and seq recovery
//! across a close/reopen. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
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

fn decode(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

fn ts_of(blob: &[u8]) -> (u32, u32) {
    match decode(blob).get("ts") {
        Some(Bson::Timestamp(t)) => (t.time, t.increment),
        other => panic!("expected ts Timestamp, got {other:?}"),
    }
}

#[test]
fn insert_emits_oplog_insert_entry() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "x": 7}))
            .unwrap();
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(rows.len(), 1);
        let (seq, blob) = &rows[0];
        assert_eq!(*seq, 1);
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
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(
            rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        for w in rows.windows(2) {
            assert!(
                ts_of(&w[1].1) > ts_of(&w[0].1),
                "timestamps must strictly increase"
            );
        }
        assert_eq!(st.oplog_tail_seq(), 3);
        assert_eq!(st.oplog_floor_seq().unwrap(), 1);
    });
}

#[test]
fn read_oplog_honours_start_and_limit() {
    with_db(|st| {
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        let from3 = st.read_oplog(3, 100).unwrap();
        assert_eq!(
            from3.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        let limited = st.read_oplog(1, 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].0, 1);
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
        assert!(st.read_oplog(1, 10).unwrap().is_empty());
        assert_eq!(st.oplog_floor_seq().unwrap(), 0);
        assert_eq!(st.oplog_tail_seq(), 0);
    });
}

#[test]
fn reopen_recovers_seq_counter() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        assert_eq!(st.oplog_tail_seq(), 3);
    }
    {
        // No meta row was persisted (current_cluster_time was never called), so
        // recovery falls back to scanning the oplog table for the max seq.
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        assert_eq!(st.oplog_tail_seq(), 3);
        st.insert_one("app", "c", &enc(&doc! {"_id": 4})).unwrap();
        let rows = st.read_oplog(4, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 4); // continues at seq 4, no collision
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
        let rows = st.read_oplog(1, 100).unwrap();
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
        let rows = st.read_oplog(1, 100).unwrap();
        assert_eq!(rows.len(), 2);
        let d = decode(&rows[1].1);
        assert_eq!(d.get_str("op").unwrap(), "d");
        assert_eq!(d.get_str("ns").unwrap(), "app.c");
        assert_eq!(d.get_document("o").unwrap().get_i32("_id").unwrap(), 7);
        assert_eq!(d.get_document("o2").unwrap().get_i32("_id").unwrap(), 7);
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
        let ops: Vec<String> = st
            .read_oplog(1, 100)
            .unwrap()
            .iter()
            .map(|(_, b)| decode(b).get_str("op").unwrap().to_string())
            .collect();
        assert_eq!(ops, vec!["i", "u", "d"]);
    });
}
