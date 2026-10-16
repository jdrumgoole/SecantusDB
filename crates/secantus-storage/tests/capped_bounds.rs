//! Capped-collection eviction against a model (RM-111). Bringing a capped
//! collection back within bounds no longer reads the whole collection on each
//! insert: it keeps the size and count from the last insert, and recounts
//! when anything else wrote the collection since. These tests hold the
//! eviction to a FIFO model through every way the kept tally could go stale:
//! updates that resize a document, deletes, a `collMod` of the bounds, a
//! rename away and back, a drop and recreate, a reopen, and concurrent
//! writers. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-capped-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn dec(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

/// `(_id, stored size)` of every document, in natural order.
fn stored(st: &Storage, db: &str, coll: &str) -> Vec<(i64, i64)> {
    st.find_matching(db, coll, &Document::new())
        .unwrap()
        .iter()
        .map(|b| (dec(b).get_i64("_id").unwrap(), b.len() as i64))
        .collect()
}

/// A capped collection as mongod keeps it: FIFO, evicted after each insert
/// until within `size` bytes and `max` documents, the newest always kept.
struct Model {
    docs: VecDeque<(i64, i64)>,
    size: i64,
    max: i64,
}

impl Model {
    fn evict(&mut self) {
        let over = |m: &Model| {
            let total: i64 = m.docs.iter().map(|(_, n)| n).sum();
            total > m.size || m.docs.len() as i64 > m.max
        };
        while self.docs.len() > 1 && over(self) {
            self.docs.pop_front();
        }
    }
}

fn doc_of(id: i64, pad: usize) -> Vec<u8> {
    enc(&doc! { "_id": id, "pad": "x".repeat(pad) })
}

fn insert(st: &Storage, m: &mut Model, coll: &str, batch: &[(i64, usize)]) {
    let docs: Vec<Vec<u8>> = batch.iter().map(|(id, pad)| doc_of(*id, *pad)).collect();
    for d in &docs {
        m.docs
            .push_back((dec(d).get_i64("_id").unwrap(), d.len() as i64));
    }
    let (n, errs) = st.insert("db", coll, docs, true).unwrap();
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(n, batch.len());
    m.evict();
}

fn check(st: &Storage, m: &Model, coll: &str, step: &str) {
    let want: Vec<(i64, i64)> = m.docs.iter().copied().collect();
    assert_eq!(stored(st, "db", coll), want, "after {step}");
}

/// A small deterministic generator, so a failure reproduces.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn create_capped(st: &Storage, coll: &str, size: i64, max: i64) {
    st.create_collection_with_options(
        "db",
        coll,
        &doc! { "capped": true, "size": size, "max": max },
    )
    .unwrap();
}

#[test]
fn eviction_follows_the_model_through_resizing_updates_and_deletes() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    create_capped(&st, "c", 6_000, 40);
    let mut m = Model {
        docs: VecDeque::new(),
        size: 6_000,
        max: 40,
    };
    let mut rng = Lcg(7);
    let mut next_id = 0i64;
    for step in 0..400 {
        match rng.next(10) {
            // Mostly inserts, of one or several documents.
            0..=5 => {
                let n = 1 + rng.next(5) as usize;
                let batch: Vec<(i64, usize)> = (0..n)
                    .map(|_| {
                        next_id += 1;
                        (next_id, rng.next(300) as usize)
                    })
                    .collect();
                insert(&st, &mut m, "c", &batch);
            }
            // A replacement that changes the document's size.
            6 | 7 if !m.docs.is_empty() => {
                let i = rng.next(m.docs.len() as u64) as usize;
                let id = m.docs[i].0;
                let new = doc_of(id, rng.next(600) as usize);
                assert!(st.replace_by_id("db", "c", &Bson::Int64(id), &new).unwrap());
                m.docs[i].1 = new.len() as i64;
            }
            8 if !m.docs.is_empty() => {
                let i = rng.next(m.docs.len() as u64) as usize;
                let id = m.docs[i].0;
                assert!(st.delete_by_id("db", "c", &Bson::Int64(id)).unwrap());
                m.docs.remove(i);
            }
            _ => {}
        }
        check(&st, &m, "c", &format!("step {step}"));
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_coll_mod_of_the_bounds_applies_on_the_next_insert() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    create_capped(&st, "c", 1_000_000, 50);
    let mut m = Model {
        docs: VecDeque::new(),
        size: 1_000_000,
        max: 50,
    };
    for id in 1..=60 {
        insert(&st, &mut m, "c", &[(id, 10)]);
    }
    check(&st, &m, "c", "fill");
    st.coll_mod("db", "c", &doc! { "max": 20i64 }).unwrap();
    m.max = 20;
    insert(&st, &mut m, "c", &[(61, 10)]);
    check(&st, &m, "c", "collMod cappedMax");
    let size = 15 * m.docs[0].1;
    st.coll_mod("db", "c", &doc! { "size": size }).unwrap();
    m.size = size;
    insert(&st, &mut m, "c", &[(62, 10)]);
    check(&st, &m, "c", "collMod cappedSize");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rename_drop_and_reopen_recount_the_collection() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    create_capped(&st, "c", 1_000_000, 10);
    let mut m = Model {
        docs: VecDeque::new(),
        size: 1_000_000,
        max: 10,
    };
    for id in 1..=15 {
        insert(&st, &mut m, "c", &[(id, 10)]);
    }
    // Renamed away, written under the other name, renamed back.
    st.rename_collection("db", "c", "db", "d", false).unwrap();
    for id in 16..=20 {
        insert(&st, &mut m, "d", &[(id, 10)]);
    }
    assert!(st.delete_by_id("db", "d", &Bson::Int64(20)).unwrap());
    m.docs.pop_back();
    st.rename_collection("db", "d", "db", "c", false).unwrap();
    insert(&st, &mut m, "c", &[(21, 10)]);
    check(&st, &m, "c", "rename away and back");

    // Dropped and recreated under the same name with fewer documents.
    st.drop_collection("db", "c").unwrap();
    create_capped(&st, "c", 1_000_000, 3);
    let mut m = Model {
        docs: VecDeque::new(),
        size: 1_000_000,
        max: 3,
    };
    insert(&st, &mut m, "c", &[(100, 10)]);
    insert(&st, &mut m, "c", &[(101, 10), (102, 10), (103, 10)]);
    check(&st, &m, "c", "drop and recreate");

    // Reopened: the first insert counts the collection from the store.
    drop(st);
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    insert(&st, &mut m, "c", &[(104, 10)]);
    check(&st, &m, "c", "reopen");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn concurrent_inserts_and_deletes_leave_the_bound_exact() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    create_capped(&st, "c", 1_000_000_000, 100);
    let writers: Vec<_> = (0..4i64)
        .map(|t| {
            let st = Arc::clone(&st);
            std::thread::spawn(move || {
                for i in 0..300i64 {
                    let id = t * 1_000_000 + i;
                    st.insert("db", "c", vec![doc_of(id, 20)], true).unwrap();
                    if i % 7 == 0 {
                        // Delete one of this thread's own earlier documents,
                        // whether or not it is still there.
                        let _ = st.delete_by_id("db", "c", &Bson::Int64(id - 3)).unwrap();
                    }
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    // Whatever interleaving the threads had, an insert now leaves exactly
    // `max` documents: a tally that drifted would leave more or fewer.
    for id in 0..150i64 {
        st.insert("db", "c", vec![doc_of(10_000_000 + id, 20)], true)
            .unwrap();
        let docs = stored(&st, "db", "c");
        assert_eq!(docs.len(), 100, "after sequential insert {id}");
        assert_eq!(docs.last().unwrap().0, 10_000_000 + id);
    }
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
