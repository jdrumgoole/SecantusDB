//! Hybrid index builds (RM-187): the collection takes writes while an index
//! is built over it, and the index that commits holds exactly the keys of
//! the documents there at its commit. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{IndexBuildAbort, IndexBuildSpec, Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-hybrid-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn load(st: &Storage, n: i64) {
    for start in (0..n).step_by(1000) {
        let docs: Vec<Vec<u8>> = (start..(start + 1000).min(n))
            .map(|i| enc(&doc! {"_id": i, "a": i % 1000, "u": i, "s": format!("{i:09}")}))
            .collect();
        let (_, errors) = st.insert("app", "c", docs, true).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
    }
}

fn spec(name: &str, key: Document, options: Document) -> IndexBuildSpec {
    IndexBuildSpec {
        name: name.to_string(),
        key,
        options,
    }
}

/// Every index of `app.c` holds exactly the keys of its documents.
fn assert_consistent(st: &Storage) {
    let check = st.check_collection("app", "c").unwrap();
    for ix in check.get_array("indexes").unwrap() {
        let ix = ix.as_document().unwrap();
        assert_eq!(ix.get_i64("missing").unwrap(), 0, "{check:?}");
        assert_eq!(ix.get_i64("extra").unwrap(), 0, "{check:?}");
    }
}

fn set_a(st: &Storage, id: i64, a: i64) {
    st.update_matching(
        "app",
        "c",
        &doc! {"_id": id},
        &doc! {"$set": {"a": a}},
        false,
        false,
        &[],
        &Document::new(),
        None,
        None,
        false,
    )
    .unwrap();
}

/// A build over 100,000 documents while another thread inserts, updates
/// and deletes: the writes finish in milliseconds while the build runs,
/// and the index that commits matches the collection exactly.
#[test]
fn writes_go_on_while_an_index_is_built() {
    let home = temp_home();
    let st = Arc::new(Storage::open(home.to_str().unwrap()).unwrap());
    load(&st, 100_000);
    let uuid = [7u8; 16];
    st.begin_index_build(
        "app",
        "c",
        uuid,
        &[spec("a_1", doc! {"a": 1}, doc! {})],
        "localhost:27017",
    )
    .unwrap();
    let building = Arc::new(AtomicBool::new(true));
    let writer = {
        let (st, building) = (Arc::clone(&st), Arc::clone(&building));
        std::thread::spawn(move || {
            let mut latencies = Vec::new();
            let mut i = 0i64;
            while building.load(Ordering::Acquire) {
                let t = Instant::now();
                match i % 3 {
                    0 => {
                        let id = 1_000_000 + i;
                        let (_, e) = st
                            .insert("app", "c", vec![enc(&doc! {"_id": id, "a": -id})], true)
                            .unwrap();
                        assert!(e.is_empty());
                    }
                    1 => set_a(&st, (i * 7919) % 100_000, 5_000 + i),
                    _ => {
                        st.delete_by_id("app", "c", &Bson::Int64((i * 104_729) % 100_000))
                            .unwrap();
                    }
                }
                latencies.push(t.elapsed());
                i += 1;
            }
            latencies
        })
    };
    let started = Instant::now();
    st.run_index_build(&uuid, 200 * 1024 * 1024).unwrap();
    st.commit_index_build(&uuid).unwrap();
    let took = started.elapsed();
    building.store(false, Ordering::Release);
    let mut latencies = writer.join().unwrap();
    latencies.sort();
    let median = latencies[latencies.len() / 2];
    eprintln!(
        "build {took:?}; {} writes meanwhile, median {median:?}, max {:?}",
        latencies.len(),
        latencies.last().unwrap()
    );
    assert!(
        latencies.len() >= 20,
        "only {} writes during a {took:?} build",
        latencies.len()
    );
    assert!(
        median < Duration::from_millis(100),
        "median write {median:?}"
    );
    assert!(st
        .list_indexes("app", "c")
        .unwrap()
        .iter()
        .any(|ix| ix.get_str("name") == Ok("a_1")));
    assert!(st.index_builds(None, None).is_empty());
    assert_consistent(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A unique build over distinct values while a duplicate is inserted:
/// the insert succeeds and the build fails at its commit with mongod's
/// E11000, leaving no index, entries or recorded writes.
#[test]
fn a_duplicate_written_during_a_unique_build_fails_it_at_commit() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    load(&st, 5_000);
    let uuid = [8u8; 16];
    st.begin_index_build(
        "app",
        "c",
        uuid,
        &[spec("u_1", doc! {"u": 1}, doc! {"unique": true})],
        "localhost:27017",
    )
    .unwrap();
    let (_, e) = st
        .insert("app", "c", vec![enc(&doc! {"_id": -1, "u": 42})], true)
        .unwrap();
    assert!(e.is_empty());
    st.run_index_build(&uuid, 1 << 20).unwrap();
    let err = st.commit_index_build(&uuid).unwrap_err();
    let StorageError::DuplicateKey(c) = err else {
        panic!("{err:?}");
    };
    assert_eq!(
        c.errmsg(),
        "E11000 duplicate key error collection: app.c index: u_1 dup key: { u: 42 }"
    );
    st.abort_index_build(
        &uuid,
        &IndexBuildAbort::new(11000, "DuplicateKey", c.errmsg()),
    )
    .unwrap();
    assert!(st
        .list_indexes("app", "c")
        .unwrap()
        .iter()
        .all(|ix| ix.get_str("name") != Ok("u_1")));
    assert!(st.index_entries("app", "c", "u_1").unwrap().is_empty());
    assert!(st.index_builds(None, None).is_empty());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A duplicate written during the build and removed again before it
/// commits does not fail it (mongod's duplicate-key tracker checks again).
#[test]
fn a_duplicate_removed_before_commit_does_not_fail_the_build() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    load(&st, 5_000);
    let uuid = [9u8; 16];
    st.begin_index_build(
        "app",
        "c",
        uuid,
        &[spec("u_1", doc! {"u": 1}, doc! {"unique": true})],
        "localhost:27017",
    )
    .unwrap();
    st.insert("app", "c", vec![enc(&doc! {"_id": -1, "u": 42})], true)
        .unwrap();
    st.run_index_build(&uuid, 1 << 20).unwrap();
    assert!(st.delete_by_id("app", "c", &Bson::Int32(-1)).unwrap());
    st.commit_index_build(&uuid).unwrap();
    assert_consistent(&st);
    // The index enforces uniqueness from here on.
    let (_, e) = st
        .insert("app", "c", vec![enc(&doc! {"_id": -2, "u": 42})], true)
        .unwrap();
    assert_eq!(e.len(), 1, "{e:?}");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// An abort asked for stops the build at its next check; aborting it
/// removes everything it wrote.
#[test]
fn an_aborted_build_leaves_nothing() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    load(&st, 3_000);
    let uuid = [10u8; 16];
    st.begin_index_build(
        "app",
        "c",
        uuid,
        &[spec("a_1", doc! {"a": 1}, doc! {})],
        "localhost:27017",
    )
    .unwrap();
    let cause = IndexBuildAbort::new(276, "IndexBuildAborted", "dropIndexes command");
    assert_eq!(
        st.request_index_build_abort("app", Some("c"), None, &cause),
        vec![uuid]
    );
    assert!(st.run_index_build(&uuid, 1 << 20).is_err());
    assert_eq!(st.index_build_abort_cause(&uuid), Some(cause.clone()));
    st.abort_index_build(&uuid, &cause).unwrap();
    st.wait_index_builds_gone(&[uuid]);
    assert!(st.index_entries("app", "c", "a_1").unwrap().is_empty());
    assert!(st
        .list_indexes("app", "c")
        .unwrap()
        .iter()
        .all(|ix| ix.get_str("name") != Ok("a_1")));
    assert_consistent(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A build the process stopped in the middle of is run again, from the
/// start, when the store opens -- as mongod restarts unfinished builds.
#[test]
fn an_unfinished_build_is_restarted_at_open() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        load(&st, 4_000);
        let uuid = [11u8; 16];
        st.begin_index_build(
            "app",
            "c",
            uuid,
            &[
                spec("a_1", doc! {"a": 1}, doc! {}),
                spec("s_1", doc! {"s": 1}, doc! {"unique": true}),
            ],
            "localhost:27017",
        )
        .unwrap();
        // Writes recorded for the build, then the process goes away
        // without committing it.
        set_a(&st, 3, 5555);
        st.delete_by_id("app", "c", &Bson::Int64(4)).unwrap();
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let names: Vec<String> = st
        .list_indexes("app", "c")
        .unwrap()
        .iter()
        .map(|ix| ix.get_str("name").unwrap().to_string())
        .collect();
    assert_eq!(names, ["_id_", "a_1", "s_1"]);
    assert!(st.index_builds(None, None).is_empty());
    assert_consistent(&st);
    let hit = st.find_matching("app", "c", &doc! {"a": 5555}).unwrap();
    assert_eq!(hit.len(), 1);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A build whose sorter's budget is far smaller than its keys sorts
/// externally and still builds the exact index.
#[test]
fn a_build_over_its_memory_budget_sorts_externally() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    load(&st, 20_000);
    let uuid = [12u8; 16];
    st.begin_index_build(
        "app",
        "c",
        uuid,
        &[spec("s_1", doc! {"s": 1}, doc! {})],
        "localhost:27017",
    )
    .unwrap();
    st.run_index_build(&uuid, 64 * 1024).unwrap();
    st.commit_index_build(&uuid).unwrap();
    assert_eq!(st.index_entries("app", "c", "s_1").unwrap().len(), 20_000);
    assert_consistent(&st);
    let tmp = home.join("_tmp");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|rd| rd.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "spilled runs left behind: {left:?}");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// `--repair` over a store a build was cut off in: the repair's open
/// restarts and commits the build (RM-187) before it checks every
/// collection, and the index it leaves agrees with the documents.
#[test]
fn a_repair_finishes_an_unfinished_build() {
    let home = temp_home();
    let opts = secantus_storage::StorageOptions {
        durable: Some(true),
        ..Default::default()
    };
    {
        let st = Storage::open_with_options(home.to_str().unwrap(), &opts).unwrap();
        load(&st, 3_000);
        st.begin_index_build(
            "app",
            "c",
            [13u8; 16],
            &[spec("a_1", doc! {"a": 1}, doc! {})],
            "localhost:27017",
        )
        .unwrap();
        set_a(&st, 7, 9_999);
    }
    let report =
        secantus_storage::repair::repair(home.to_str().unwrap(), &opts, &mut |_| {}).unwrap();
    assert!(report.salvaged.is_empty(), "{:?}", report.salvaged);
    let st = Storage::open_with_options(home.to_str().unwrap(), &opts).unwrap();
    assert!(st
        .list_indexes("app", "c")
        .unwrap()
        .iter()
        .any(|ix| ix.get_str("name") == Ok("a_1")));
    assert!(st.index_builds(None, None).is_empty());
    assert_consistent(&st);
    assert_eq!(
        st.find_matching("app", "c", &doc! {"a": 9_999})
            .unwrap()
            .len(),
        1
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
