//! `compact` and `autoCompact` (RM-192, RM-219) against real WiredTiger: a
//! collection most of whose documents were deleted gives its space back,
//! its documents and indexes intact, also while other sessions write to it
//! and to the collections it shares files with; a dry run, a free-space
//! target above what is free, and a store with nothing to give back free
//! nothing; and the background server compacts, reports itself in the
//! engine statistics and refuses a second configuration while it runs.

use bson::{doc, spec::BinarySubtype, Binary, Bson, Document};
use secantus_storage::compact::{AutoCompactOutcome, CompactOutcome, CompactTarget};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-compact-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(home: &std::path::Path) -> Storage {
    let cfg = secantus_storage::wt_config("256M", 1000, false, "10MB");
    Storage::open_with_config(home.to_str().unwrap(), &cfg).unwrap()
}

/// Opened as `secantusd-rs` opens its store: data tables unlogged and
/// timestamped, durable through checkpoints and oplog replay.
fn open_like_the_server(home: &std::path::Path) -> Storage {
    let opts = secantus_storage::StorageOptions {
        wt_config: Some(secantus_storage::wt_config("256M", 1000, false, "10MB")),
        data_nonlogged: Some(true),
        transactions_logged_at_commit: true,
        ..Default::default()
    };
    Storage::open_with_options(home.to_str().unwrap(), &opts).unwrap()
}

/// Bytes that do not compress, so the files hold what was written.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn document(i: i64) -> Vec<u8> {
    bson::to_vec(&doc! {
        "_id": i,
        "k": i % 97,
        "p": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: noise(i as u64, 2000) }),
    })
    .unwrap()
}

/// `n` documents of ~2 KB of noise with an index on `k`, then all but every
/// tenth deleted and a checkpoint taken, so the deleted pages are free.
fn mostly_deleted(st: &Storage, db: &str, coll: &str, n: i64) {
    st.create_index(db, coll, "k_1", &doc! {"k": 1}, &doc! {})
        .unwrap();
    for start in (0..n).step_by(1000) {
        let docs: Vec<Vec<u8>> = (start..(start + 1000).min(n)).map(document).collect();
        st.insert(db, coll, docs, true).unwrap();
    }
    st.checkpoint_now().unwrap();
    let deleted = st
        .delete_matching(
            db,
            coll,
            &doc! {"$expr": {"$ne": [{"$mod": ["$_id", 10]}, 0]}},
            0,
            &Document::new(),
            None,
        )
        .unwrap();
    assert_eq!(deleted as i64, n - (n + 9) / 10);
    st.checkpoint_now().unwrap();
}

fn tables_size(st: &Storage, home: &std::path::Path, target: CompactTarget<'_>) -> u64 {
    st.compact_tables(target)
        .unwrap()
        .iter()
        .map(|t| {
            std::fs::metadata(home.join(format!("{}.wt", t.trim_start_matches("table:"))))
                .unwrap()
                .len()
        })
        .sum()
}

/// Every document is readable and every index agrees with the documents.
fn intact(st: &Storage, db: &str, coll: &str, expect: i64) {
    let check = st.check_collection(db, coll).unwrap();
    assert_eq!(check.get_i64("records").unwrap(), expect, "{check:?}");
    assert_eq!(check.get_i64("undecodable").unwrap(), 0, "{check:?}");
    let rendered = format!("{check:?}");
    for index in check.get_array("indexes").into_iter().flatten() {
        let index = index.as_document().unwrap();
        for field in ["missing", "extra"] {
            if let Ok(n) = index.get_i64(field) {
                assert_eq!(n, 0, "{field} entries: {rendered}");
            }
        }
    }
}

#[test]
fn compact_gives_a_collections_free_space_back() {
    let home = temp_home();
    let st = open(&home);
    mostly_deleted(&st, "app", "big", 20_000);
    let target = CompactTarget::Collection("app", "big");
    let tables = st.compact_tables(target).unwrap();
    assert!(tables
        .iter()
        .any(|t| t.starts_with("table:secantus_documents_sh")));
    assert!(tables.contains(&"table:secantus_index_entries".to_string()));
    let before = tables_size(&st, &home, target);

    // A dry run compacts nothing (mongod 8.2.11 estimated 0 on every shape
    // measured), and a target above the free space skips every file.
    assert_eq!(
        st.compact(target, true, None).unwrap(),
        CompactOutcome::Freed(0)
    );
    assert_eq!(
        st.compact(target, false, Some(100_000)).unwrap(),
        CompactOutcome::Freed(0)
    );
    assert_eq!(tables_size(&st, &home, target), before);

    let CompactOutcome::Freed(freed) = st.compact(target, false, None).unwrap() else {
        panic!("compact did not run");
    };
    let after = tables_size(&st, &home, target);
    assert!(freed > 20_000_000, "freed {freed} of {before}");
    assert_eq!(freed as u64, before - after);
    intact(&st, "app", "big", 2_000);
    assert_eq!(
        st.find_by_id("app", "big", &Bson::Int64(1230)).unwrap(),
        Some(document(1230))
    );

    // Nothing left to give back.
    assert_eq!(
        st.compact(target, false, None).unwrap(),
        CompactOutcome::Freed(0)
    );
    drop(st);

    // The compacted store reopens with everything in it.
    let st = open(&home);
    intact(&st, "app", "big", 2_000);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn compact_of_an_empty_store_or_an_in_memory_one_frees_nothing() {
    let home = temp_home();
    let st = open(&home);
    st.insert("app", "small", vec![document(1)], true).unwrap();
    let target = CompactTarget::Collection("app", "small");
    assert_eq!(
        st.compact(target, false, Some(1)).unwrap(),
        CompactOutcome::Freed(0)
    );
    assert_eq!(
        st.compact(CompactTarget::Catalog, false, None).unwrap(),
        CompactOutcome::Freed(0)
    );
    assert_eq!(
        st.compact(CompactTarget::Oplog, false, Some(1)).unwrap(),
        CompactOutcome::Freed(0)
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);

    let home = temp_home();
    let mem = Storage::open_with_config(
        home.to_str().unwrap(),
        "create,in_memory=true,cache_size=64M,statistics=(fast)",
    )
    .unwrap();
    mem.insert("app", "c", vec![document(1)], true).unwrap();
    assert_eq!(
        mem.compact(CompactTarget::Collection("app", "c"), false, None)
            .unwrap(),
        CompactOutcome::Freed(0)
    );
    assert_eq!(
        mem.auto_compact(true, false, None).unwrap(),
        AutoCompactOutcome::InMemory
    );
    drop(mem);
    let _ = std::fs::remove_dir_all(&home);
}

/// Compaction is online: writers to the collection, and to another
/// collection in the same document shard, carry on while it runs, and
/// every document either wrote is there afterwards with its indexes whole.
#[test]
fn compact_runs_while_the_collection_and_its_neighbours_are_written() {
    written_during_compaction(open);
}

#[test]
fn compact_runs_while_written_in_a_store_opened_as_the_server_opens_it() {
    written_during_compaction(open_like_the_server);
}

fn written_during_compaction(open: fn(&std::path::Path) -> Storage) {
    let home = temp_home();
    let st = Arc::new(open(&home));
    mostly_deleted(&st, "app", "big", 20_000);
    // A neighbour: a collection whose documents live in the same shard.
    let shard = secantus_storage::collection_table("app", "big");
    let neighbour = (0..10_000)
        .map(|i| format!("n{i}"))
        .find(|c| secantus_storage::collection_table("app", c) == shard)
        .unwrap();
    st.create_index("app", &neighbour, "k_1", &doc! {"k": 1}, &doc! {})
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = |coll: String, base: i64| {
        let st = st.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut i = base;
            while !stop.load(Ordering::Acquire) {
                st.insert("app", &coll, vec![document(i)], true).unwrap();
                i += 1;
            }
            i - base
        })
    };
    let same = writer("big".to_string(), 1_000_000);
    let other = writer(neighbour.clone(), 0);
    // Let both writers get going before compaction starts.
    std::thread::sleep(Duration::from_millis(200));
    let outcome = st
        .compact(CompactTarget::Collection("app", "big"), false, Some(1))
        .unwrap();
    stop.store(true, Ordering::Release);
    let wrote_same = same.join().unwrap();
    let wrote_other = other.join().unwrap();
    assert!(matches!(outcome, CompactOutcome::Freed(_)), "{outcome:?}");
    assert!(wrote_same > 0 && wrote_other > 0);
    intact(&st, "app", "big", 2_000 + wrote_same);
    intact(&st, "app", &neighbour, wrote_other);
    for i in [1_000_000, 1_000_000 + wrote_same - 1] {
        assert_eq!(
            st.find_by_id("app", "big", &Bson::Int64(i)).unwrap(),
            Some(document(i))
        );
    }
    let st = Arc::try_unwrap(st).ok().unwrap();
    st.close().unwrap();
    drop(st);
    let st = open(&home);
    intact(&st, "app", "big", 2_000 + wrote_same);
    intact(&st, "app", &neighbour, wrote_other);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// Two `compact`s of one collection at once: the one that comes second is
/// refused while the other runs (mongod: "Compaction is already in
/// progress"), and a dry run claims the collection as a compaction does.
#[test]
fn a_second_compact_of_the_same_collection_is_refused_while_one_runs() {
    let home = temp_home();
    let st = Arc::new(open(&home));
    let mut refused = false;
    // The first compact takes a while (it rewrites ~40 MB); try the second
    // until one lands inside it.
    for attempt in 0..5 {
        let coll = format!("big{attempt}");
        mostly_deleted(&st, "app", &coll, 20_000);
        let first = {
            let st = st.clone();
            let coll = coll.clone();
            std::thread::spawn(move || {
                st.compact(CompactTarget::Collection("app", &coll), false, Some(1))
                    .unwrap()
            })
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !first.is_finished() && Instant::now() < deadline {
            if st
                .compact(CompactTarget::Collection("app", &coll), true, None)
                .unwrap()
                == CompactOutcome::AlreadyRunning
            {
                refused = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Either side can be the one refused: a dry run holds its claim too.
        if first.join().unwrap() == CompactOutcome::AlreadyRunning {
            refused = true;
        }
        // Other collections are not held up by it.
        assert_eq!(
            st.compact(CompactTarget::Collection("app", "other"), true, None)
                .unwrap(),
            CompactOutcome::Freed(0)
        );
        if refused {
            break;
        }
    }
    assert!(refused, "no second compact ever met the first");
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// `autoCompact`, once the server has taken up the previous change: until
/// it has, 11.3 refuses any other (`Busy`), and that refusal is a timing,
/// not what these tests are about.
fn ask(st: &Storage, enable: bool, run_once: bool, target: Option<i64>) -> AutoCompactOutcome {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match st.auto_compact(enable, run_once, target).unwrap() {
            AutoCompactOutcome::Busy(_) => {
                assert!(
                    Instant::now() < deadline,
                    "the server never took up a change"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

fn background(st: &Storage) -> Vec<(String, i64)> {
    st.wt_statistics()
        .unwrap()
        .1
        .into_iter()
        .filter(|(d, _)| d.starts_with("background-compact: "))
        .collect()
}

fn stat(st: &Storage, name: &str) -> i64 {
    background(st)
        .into_iter()
        .find(|(d, _)| d.ends_with(name))
        .unwrap_or_else(|| panic!("no statistic {name}"))
        .1
}

#[test]
fn auto_compact_compacts_in_the_background_and_reports_itself() {
    let home = temp_home();
    let st = open(&home);
    // The counters are there before it is ever enabled, all zero, in
    // WiredTiger 11.3's order and its place among the statistics.
    let names: Vec<String> = background(&st).into_iter().map(|(d, _)| d).collect();
    assert_eq!(names, secantus_storage::compact::BACKGROUND_STATISTICS);
    let all = st.wt_statistics().unwrap().1;
    let first = all
        .iter()
        .position(|(d, _)| d.starts_with("background-compact: "))
        .unwrap();
    assert!(first == 0 || all[first - 1].0.as_str() < names[0].as_str());
    assert!(all[first + names.len()..]
        .iter()
        .all(|(d, _)| d.as_str() > names[0].as_str()));
    assert!(background(&st).iter().all(|(_, v)| *v == 0));

    // Disabling what is off, and enabling twice alike, do nothing.
    assert_eq!(ask(&st, false, false, None), AutoCompactOutcome::Done);
    mostly_deleted(&st, "app", "big", 20_000);
    let target = CompactTarget::Collection("app", "big");
    let before = tables_size(&st, &home, target);

    assert_eq!(ask(&st, true, true, Some(1)), AutoCompactOutcome::Done);
    // runOnce: one pass over every file, then it stops by itself.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let done = stat(&st, "running") == 0 && stat(&st, "successful calls") > 0;
        if done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "runOnce never finished: {:?}",
            background(&st)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        stat(&st, "recovered bytes") > 20_000_000,
        "{:?}",
        background(&st)
    );
    assert!(stat(&st, "skipped file as it is part of the exclude list") > 0);
    assert!(tables_size(&st, &home, target) < before / 2);
    intact(&st, "app", "big", 2_000);

    // On, then a different configuration while on is refused; the same is
    // not; off is off.
    assert_eq!(ask(&st, true, false, Some(5)), AutoCompactOutcome::Done);
    assert_eq!(ask(&st, true, false, Some(5)), AutoCompactOutcome::Done);
    assert_eq!(
        ask(&st, true, false, None),
        AutoCompactOutcome::AlreadyRunning
    );
    assert_eq!(
        ask(&st, true, true, Some(5)),
        AutoCompactOutcome::AlreadyRunning
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while stat(&st, "running") != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(ask(&st, false, false, None), AutoCompactOutcome::Done);
    assert_eq!(ask(&st, true, false, None), AutoCompactOutcome::Done);
    // Closing stops it.
    st.close().unwrap();
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// The background server compacting while writers run: nothing written is
/// lost, the indexes stay whole, and closing stops it cleanly.
#[test]
fn auto_compact_runs_beside_writers() {
    let home = temp_home();
    let st = Arc::new(open(&home));
    mostly_deleted(&st, "app", "big", 20_000);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let st = st.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut i = 1_000_000i64;
            while !stop.load(Ordering::Acquire) {
                st.insert("app", "big", vec![document(i)], true).unwrap();
                if i % 3 == 0 {
                    st.delete_by_id("app", "big", &Bson::Int64(i - 1)).unwrap();
                }
                i += 1;
            }
            i
        })
    };
    assert_eq!(ask(&st, true, false, Some(1)), AutoCompactOutcome::Done);
    let deadline = Instant::now() + Duration::from_secs(120);
    while stat(&st, "recovered bytes") == 0 {
        assert!(Instant::now() < deadline, "{:?}", background(&st));
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Release);
    let end = writer.join().unwrap();
    let mut expect = 2_000i64;
    for i in 1_000_000..end {
        let deleted = (i + 1) % 3 == 0 && i + 1 < end;
        if !deleted {
            expect += 1;
        }
    }
    intact(&st, "app", "big", expect);
    let st = Arc::try_unwrap(st).ok().unwrap();
    st.close().unwrap();
    drop(st);
    let st = open(&home);
    intact(&st, "app", "big", expect);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
