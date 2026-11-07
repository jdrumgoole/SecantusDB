//! A statement's view of what the user code it called wrote
//! (`StatementViews`, RP-473): once a level has called a volatile function,
//! its reads see each row the call wrote as it was before the call -- an
//! inserted row absent, an updated one at its old value, a deleted one
//! still there -- while the call's own statements, and the next client
//! statement, see the writes. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{HeldRows, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home: PathBuf =
        std::env::temp_dir().join(format!("secantus-views-{}-{}", std::process::id(), n));
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

/// `(id, v)` of every row a read of `c` sees, in `_id` order.
fn rows(st: &Storage) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = st
        .find_matching("app", "c", &Document::new())
        .unwrap()
        .iter()
        .map(|b| {
            let d: Document = bson::from_slice(b).unwrap();
            (d.get_i32("_id").unwrap(), d.get_i32("v").unwrap())
        })
        .collect();
    out.sort_unstable();
    out
}

fn v_of(st: &Storage, id: i32) -> Option<i32> {
    st.find_by_id("app", "c", &Bson::Int32(id))
        .unwrap()
        .map(|b| {
            bson::from_slice::<Document>(&b)
                .unwrap()
                .get_i32("v")
                .unwrap()
        })
}

/// The call's writes: an insert, an update and a delete.
fn call_writes(st: &Storage) {
    st.insert_one("app", "c", &enc(&doc! {"_id": 7, "v": 7}))
        .unwrap();
    st.replace_by_id("app", "c", &Bson::Int32(2), &enc(&doc! {"_id": 2, "v": 20}))
        .unwrap();
    st.delete_by_id("app", "c", &Bson::Int32(3)).unwrap();
}

fn seeded(st: &Storage) {
    for i in 1..=3 {
        st.insert_one("app", "c", &enc(&doc! {"_id": i, "v": i}))
            .unwrap();
    }
}

const BEFORE: [(i32, i32); 3] = [(1, 1), (2, 2), (3, 3)];
const AFTER: [(i32, i32); 3] = [(1, 1), (2, 20), (7, 7)];

fn views(held: &HeldRows) -> std::sync::MutexGuard<'_, secantus_storage::Held> {
    held.lock().unwrap()
}

#[test]
fn the_calling_statement_reads_its_tables_as_they_were_before_its_calls() {
    with_db(|st| {
        seeded(st);
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        st.with_user_transaction(&mut txn, || {
            let call = views(&held).views.enter_call();
            call_writes(st);
            // Inside the call, its reads are its own statements': they see
            // what it wrote.
            assert_eq!(rows(st), AFTER);
            views(&held).views.leave_call(call);
            // The caller's reads: the rows as they were before the call.
            assert_eq!(rows(st), BEFORE);
            assert_eq!(v_of(st, 2), Some(2));
            assert_eq!(v_of(st, 3), Some(3));
            assert_eq!(v_of(st, 7), None);
            // A filter is applied to the rows as they were.
            let hits = st
                .find_matching("app", "c", &doc! {"v": {"$gte": 2}})
                .unwrap()
                .len();
            assert_eq!(hits, 2);
            // A statement of the function body run inside it sees everything.
            let level = views(&held).views.enter_statement();
            assert_eq!(rows(st), AFTER);
            views(&held).views.leave_statement(level);
            assert_eq!(rows(st), BEFORE);
            // The next client statement sees what the call wrote.
            views(&held).views.reset();
            assert_eq!(rows(st), AFTER);
        })
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}

#[test]
fn a_statements_own_writes_are_not_hidden_from_it() {
    with_db(|st| {
        seeded(st);
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        st.with_user_transaction(&mut txn, || {
            let call = views(&held).views.enter_call();
            views(&held).views.leave_call(call);
            // Written by the statement itself, after its call returned.
            st.insert_one("app", "c", &enc(&doc! {"_id": 9, "v": 9}))
                .unwrap();
            assert_eq!(rows(st), vec![(1, 1), (2, 2), (3, 3), (9, 9)]);
        })
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}

#[test]
fn a_nested_call_records_for_every_level_calling() {
    with_db(|st| {
        seeded(st);
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        st.with_user_transaction(&mut txn, || {
            let outer = views(&held).views.enter_call();
            // A statement of the outer function's body calls another.
            let level = views(&held).views.enter_statement();
            let inner = views(&held).views.enter_call();
            call_writes(st);
            views(&held).views.leave_call(inner);
            // The body's statement does not see its own call's writes ...
            assert_eq!(rows(st), BEFORE);
            views(&held).views.leave_statement(level);
            views(&held).views.leave_call(outer);
            // ... and neither does the client's.
            assert_eq!(rows(st), BEFORE);
        })
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}

#[test]
fn a_row_written_twice_is_seen_as_it_was_first() {
    with_db(|st| {
        seeded(st);
        let mut txn = st.begin_user_transaction().unwrap();
        let held = txn.held_rows();
        st.with_user_transaction(&mut txn, || {
            let call = views(&held).views.enter_call();
            st.replace_by_id("app", "c", &Bson::Int32(1), &enc(&doc! {"_id": 1, "v": 10}))
                .unwrap();
            st.replace_by_id(
                "app",
                "c",
                &Bson::Int32(1),
                &enc(&doc! {"_id": 1, "v": 100}),
            )
            .unwrap();
            views(&held).views.leave_call(call);
            assert_eq!(v_of(st, 1), Some(1));
        })
        .unwrap();
        st.rollback_user_transaction(&mut txn).unwrap();
    });
}
