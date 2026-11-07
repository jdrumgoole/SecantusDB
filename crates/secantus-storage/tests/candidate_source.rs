//! A multi-document write drawing its candidates from a source as it writes
//! them (`with_candidate_source`, RM-323): what a read that stops part-way
//! leaves written. Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::{Pulled, Storage};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-csource-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn seeded(home: &std::path::Path, n: i64) -> (Storage, Vec<(i64, Vec<u8>)>) {
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let docs: Vec<Vec<u8>> = (0..n)
        .map(|i| bson::to_vec(&doc! {"_id": i, "n": 0}).unwrap())
        .collect();
    st.insert("app", "c", docs, true).unwrap();
    let rows = st
        .records_after("app", "c", None, usize::MAX, false)
        .unwrap();
    (st, rows)
}

/// A source over `rows` that hands out `want` at a time and stops after
/// `pulls` pulls, recording each `want` it was asked for.
fn source(
    rows: Vec<(i64, Vec<u8>)>,
    pulls: usize,
    asked: &RefCell<Vec<usize>>,
) -> impl FnMut(usize, usize) -> Pulled + '_ {
    let mut rest = rows.into_iter();
    move |want, _max_bytes| {
        asked.borrow_mut().push(want);
        if asked.borrow().len() > pulls {
            return Pulled {
                rows: Vec::new(),
                exhausted: false,
                stopped: true,
            };
        }
        // `want` 0: no count limit.
        let n = if want == 0 { usize::MAX } else { want };
        let rows: Vec<_> = rest.by_ref().take(n).collect();
        Pulled {
            exhausted: rows.len() < n,
            rows,
            stopped: false,
        }
    }
}

fn update_all(st: &Storage) -> secantus_storage::Result<secantus_storage::UpdateOutcome> {
    st.update_matching(
        "app",
        "c",
        &doc! {},
        &doc! {"$inc": {"n": 1}},
        true,
        false,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
}

#[test]
fn an_update_many_keeps_the_pulls_written_before_its_read_stopped() {
    let home = temp_home();
    let (st, rows) = seeded(&home, 2500);
    let asked = RefCell::new(Vec::new());
    let mut src = source(rows, 1, &asked);
    let out = secantus_storage::with_candidate_source(&mut src, || update_all(&st));
    assert!(out.is_err(), "a read that stopped fails the write");
    // One chunk's pull was written; the second pull stopped.
    let updated = st.count_matching("app", "c", &doc! {"n": 1}, None).unwrap();
    assert_eq!(updated, asked.borrow()[0]);
    assert!(updated > 0 && updated < 2500, "{updated}");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_update_many_writes_a_row_met_twice_once() {
    let home = temp_home();
    let (st, rows) = seeded(&home, 5);
    // The read meets every row, then (as an index scan would, after the
    // write moved them ahead of it) meets them all again.
    let twice: Vec<_> = rows.iter().chain(rows.iter()).cloned().collect();
    let mut pulled = 0;
    let mut src = |_want: usize, _max: usize| {
        pulled += 1;
        Pulled {
            rows: if pulled <= 2 {
                twice[(pulled - 1) * 5..pulled * 5].to_vec()
            } else {
                Vec::new()
            },
            exhausted: pulled >= 2,
            stopped: false,
        }
    };
    let out = secantus_storage::with_candidate_source(&mut src, || update_all(&st)).unwrap();
    assert_eq!((out.matched, out.modified), (5, 5));
    assert_eq!(
        st.count_matching("app", "c", &doc! {"n": 1}, None).unwrap(),
        5
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_delete_many_keeps_the_batches_staged_before_its_read_stopped() {
    let home = temp_home();
    let (st, rows) = seeded(&home, 40);
    let asked = RefCell::new(Vec::new());
    // Two whole batches, then the third pull stops: the staged batch is lost.
    let mut src = source(rows, 2, &asked);
    let out = secantus_storage::with_reversed_delete_batches(10, || {
        secantus_storage::with_candidate_source(&mut src, || {
            st.delete_matching("app", "c", &doc! {}, 0, &Document::new(), None)
        })
    });
    assert!(out.is_err(), "a read that stopped fails the delete");
    assert_eq!(*asked.borrow(), vec![10, 10, 10], "mongod's batches of 10");
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 20);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_write_of_one_takes_the_sources_rows_as_its_candidates() {
    let home = temp_home();
    let (st, mut rows) = seeded(&home, 3);
    // A delete of one does not draw a pull at a time: it takes every row
    // the source names, in its order, and removes the first.
    rows.reverse();
    let asked = RefCell::new(Vec::new());
    let mut src = source(rows, 1, &asked);
    let deleted = secantus_storage::with_candidate_source(&mut src, || {
        st.delete_matching("app", "c", &doc! {}, 1, &Document::new(), None)
    })
    .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(
        st.count_matching("app", "c", &doc! {"_id": 2}, None)
            .unwrap(),
        0
    );
    assert_eq!(st.count_matching("app", "c", &doc! {}, None).unwrap(), 2);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
