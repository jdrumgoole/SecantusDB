//! The savepoint undo journal (`UndoJournal`, RP-018): while a server has a
//! frame open, the first write of each row since it records the row as it
//! was, and the first create or drop of a collection whether it existed.
//! With no frame open (the MongoDB server opens none) nothing is recorded.
//! Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{Storage, UndoFrame, UndoPause};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home: PathBuf =
        std::env::temp_dir().join(format!("secantus-undo-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn set_v(st: &Storage, id: i32, v: i32) {
    st.update_matching(
        "app",
        "c",
        &doc! {"_id": id},
        &doc! {"$set": {"v": v}},
        true,
        false,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
    .unwrap();
}

fn delete(st: &Storage, id: i32) {
    st.delete_matching("app", "c", &doc! {"_id": id}, 0, &Document::new(), None)
        .unwrap();
}

/// `(id, prior v)` of a frame's row records, `None` for a row that did not
/// exist.
fn priors(frame: &UndoFrame) -> Vec<(Bson, Option<i32>)> {
    frame
        .rows
        .iter()
        .map(|r| {
            let v = r.prior.as_ref().map(|p| {
                bson::from_slice::<Document>(p)
                    .unwrap()
                    .get_i32("v")
                    .unwrap()
            });
            (r.id.clone(), v)
        })
        .collect()
}

#[test]
fn first_write_since_each_savepoint_is_recorded() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": 1}))
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        held.lock().unwrap().undo.open(10);
        st.with_user_transaction(&mut txn, || {
            set_v(st, 1, 2);
            set_v(st, 1, 20);
            st.insert_one("app", "c", &enc(&doc! {"_id": 2, "v": 2}))
                .unwrap();
        })
        .unwrap();
        held.lock().unwrap().undo.open(11);
        st.with_user_transaction(&mut txn, || {
            set_v(st, 1, 3);
            delete(st, 1);
            delete(st, 2);
        })
        .unwrap();
        // The inner savepoint's records: each row as it was when it was taken.
        let inner = held.lock().unwrap().undo.rollback_to(11).unwrap();
        assert_eq!(
            priors(&inner),
            vec![(Bson::Int32(1), Some(20)), (Bson::Int32(2), Some(2))]
        );
        // Still open, and empty.
        assert!(held.lock().unwrap().undo.has_frame(11));
        // The outer one: the oldest record of each row among it and the
        // frames inside it.
        st.with_user_transaction(&mut txn, || set_v(st, 2, 99))
            .unwrap();
        let outer = held.lock().unwrap().undo.rollback_to(10).unwrap();
        assert_eq!(
            priors(&outer),
            vec![(Bson::Int32(1), Some(1)), (Bson::Int32(2), None)]
        );
        assert!(!held.lock().unwrap().undo.has_frame(11));
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}

#[test]
fn release_merges_into_the_enclosing_frame_oldest_first() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": 1}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "v": 2}))
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        held.lock().unwrap().undo.open(1);
        st.with_user_transaction(&mut txn, || set_v(st, 1, 10))
            .unwrap();
        held.lock().unwrap().undo.open(2);
        st.with_user_transaction(&mut txn, || {
            set_v(st, 1, 100);
            set_v(st, 2, 200);
        })
        .unwrap();
        assert!(held.lock().unwrap().undo.release(2));
        let frame = held.lock().unwrap().undo.rollback_to(1).unwrap();
        assert_eq!(
            priors(&frame),
            vec![(Bson::Int32(1), Some(1)), (Bson::Int32(2), Some(2))]
        );
        assert!(!held.lock().unwrap().undo.release(2));
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}

#[test]
fn nothing_is_recorded_without_a_frame_or_while_paused() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "v": 1}))
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        // No frame: a MongoDB-server transaction.
        st.with_user_transaction(&mut txn, || set_v(st, 1, 2))
            .unwrap();
        held.lock().unwrap().undo.open(7);
        {
            let _paused = UndoPause::new(&held);
            st.with_user_transaction(&mut txn, || set_v(st, 1, 3))
                .unwrap();
        }
        let frame = held.lock().unwrap().undo.rollback_to(7).unwrap();
        assert!(frame.rows.is_empty() && frame.collections.is_empty());
        // Recording resumes once the pause is over.
        st.with_user_transaction(&mut txn, || set_v(st, 1, 4))
            .unwrap();
        let frame = held.lock().unwrap().undo.rollback_to(7).unwrap();
        assert_eq!(priors(&frame), vec![(Bson::Int32(1), Some(3))]);
        st.rollback_user_transaction(&mut txn).unwrap();
        // An autocommit write is no transaction's.
        st.insert_one("app", "c", &enc(&doc! {"_id": 5, "v": 5}))
            .unwrap();
    });
}

#[test]
fn a_dropped_collection_records_every_row_and_that_it_existed() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "v": i}))
                .unwrap();
        }
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        held.lock().unwrap().undo.open(1);
        st.with_user_transaction(&mut txn, || {
            set_v(st, 2, 20);
            st.drop_collection("app", "c").unwrap();
            st.create_collection("app", "d").unwrap();
        })
        .unwrap();
        let frame = held.lock().unwrap().undo.rollback_to(1).unwrap();
        let mut got = priors(&frame);
        got.sort_by_key(|(id, _)| id.as_i32());
        assert_eq!(
            got,
            vec![
                (Bson::Int32(1), Some(1)),
                (Bson::Int32(2), Some(2)),
                (Bson::Int32(3), Some(3)),
            ]
        );
        let colls: Vec<(String, bool)> = frame
            .collections
            .iter()
            .map(|c| (c.coll.clone(), c.existed))
            .collect();
        assert_eq!(
            colls,
            vec![("c".to_string(), true), ("d".to_string(), false)]
        );
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}
