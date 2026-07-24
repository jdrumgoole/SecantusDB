//! Natural (insertion) order index: an unsorted `find` returns docs in insertion
//! order — not `_id`-sort order — even for mixed `_id` types inserted out of
//! order. The order survives reopen (counter recovery), and delete+reinsert /
//! drop+recreate don't double or resurrect rows. Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-nat-{}-{}", std::process::id(), n));
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

/// `_id`s of an unsorted `find` (i.e. natural order).
fn ids(st: &Storage, db: &str, coll: &str) -> Vec<Bson> {
    st.find_matching(db, coll, &Document::new())
        .unwrap()
        .iter()
        .map(|b| dec(b).get("_id").cloned().unwrap())
        .collect()
}

#[test]
fn unsorted_find_returns_insertion_order_for_mixed_id_types() {
    let home = temp_home();
    let oid = bson::oid::ObjectId::new();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        // Insertion order (1, ObjectId, "foo", "bar") differs from BSON `_id`-sort
        // order (1, "bar", "foo", ObjectId) — the case php-lib testInserts pins.
        let docs = vec![
            enc(&doc! {"_id": 1, "x": 11}),
            enc(&doc! {"_id": oid, "x": 22}),
            enc(&doc! {"_id": "foo", "x": 33}),
            enc(&doc! {"_id": "bar", "x": 44}),
        ];
        st.insert("app", "c", docs, true).unwrap();
        assert_eq!(
            ids(&st, "app", "c"),
            vec![
                Bson::Int32(1),
                Bson::ObjectId(oid),
                Bson::String("foo".into()),
                Bson::String("bar".into()),
            ],
        );
    }
    // Reopen: insertion order preserved, and a new insert lands at the end.
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        let got = ids(&st, "app", "c");
        assert_eq!(got[0], Bson::Int32(1));
        assert_eq!(got[3], Bson::String("bar".into()));
        st.insert("app", "c", vec![enc(&doc! {"_id": 0})], true)
            .unwrap();
        assert_eq!(ids(&st, "app", "c").last().unwrap(), &Bson::Int32(0));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn delete_then_reinsert_same_id_does_not_double() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert(
        "app",
        "c",
        vec![
            enc(&doc! {"_id": "a"}),
            enc(&doc! {"_id": "b"}),
            enc(&doc! {"_id": "c"}),
        ],
        true,
    )
    .unwrap();
    st.delete_by_id("app", "c", &Bson::String("a".into()))
        .unwrap();
    // Reinsert "a" — exactly once, at the new (end) position.
    st.insert("app", "c", vec![enc(&doc! {"_id": "a"})], true)
        .unwrap();
    assert_eq!(
        ids(&st, "app", "c"),
        vec![
            Bson::String("b".into()),
            Bson::String("c".into()),
            Bson::String("a".into()),
        ],
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn drop_and_recreate_resets_natural_order() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert(
        "app",
        "c",
        vec![enc(&doc! {"_id": "z"}), enc(&doc! {"_id": "a"})],
        true,
    )
    .unwrap();
    assert_eq!(
        ids(&st, "app", "c"),
        vec![Bson::String("z".into()), Bson::String("a".into())]
    );
    st.drop_collection("app", "c").unwrap();
    // Re-create with the same `_id`s in the opposite order — no resurrection.
    st.insert(
        "app",
        "c",
        vec![enc(&doc! {"_id": "a"}), enc(&doc! {"_id": "z"})],
        true,
    )
    .unwrap();
    assert_eq!(
        ids(&st, "app", "c"),
        vec![Bson::String("a".into()), Bson::String("z".into())]
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn natural_hint_walks_insertion_order() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert(
        "app",
        "c",
        vec![
            enc(&doc! {"_id": 5}),
            enc(&doc! {"_id": 1}),
            enc(&doc! {"_id": 3}),
        ],
        true,
    )
    .unwrap();
    let hint = secantus_storage::Hint::Name("$natural".to_string());
    let got: Vec<Bson> = st
        .find_matching_with(
            "app",
            "c",
            &Document::new(),
            None,
            Some(&hint),
            None,
            &Document::new(),
        )
        .unwrap()
        .iter()
        .map(|b| dec(b).get("_id").cloned().unwrap())
        .collect();
    assert_eq!(got, vec![Bson::Int32(5), Bson::Int32(1), Bson::Int32(3)]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// The tailable capped-cursor primitives follow **insertion (RecordId) order**,
/// not `_id`/`id_key` order — the step-3 fix. Regression guard for the bug step 1
/// introduced: a capped tailable cursor tracked position by `id_key`, so on a
/// collection with custom **non-monotonic** `_id`s a later insert carrying a
/// smaller `_id` sorted *before* the watermark and was silently dropped. The
/// RecordId-ordered scan cannot drop it: RecordId is the monotonic insertion
/// counter regardless of `_id` value.
#[test]
fn tailable_recordid_scan_follows_insertion_order_for_nonmonotonic_ids() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    // Insert with DESCENDING _ids so id_key order is the REVERSE of insertion.
    for id in [50i32, 40, 30, 20, 10] {
        st.insert("app", "c", vec![enc(&doc! {"_id": id})], false)
            .unwrap();
    }
    // Whole-collection RecordId scan = insertion order (50,40,30,20,10), NOT
    // _id-ascending (10,20,30,40,50) that an id_key scan would give.
    let all: Vec<i32> = st
        .scan_docs_after_recordid("app", "c", None)
        .unwrap()
        .iter()
        .map(|(_rid, b)| dec(b).get_i32("_id").unwrap())
        .collect();
    assert_eq!(
        all,
        vec![50, 40, 30, 20, 10],
        "scan must be insertion order"
    );

    // Anchor at the 3rd doc's RecordId; the tail is the LATER inserts (20,10) —
    // both have SMALLER _ids than the anchor's, so an id_key `> after` filter
    // would have dropped them. RecordId order keeps them.
    let after_third = st.scan_docs_after_recordid("app", "c", None).unwrap()[2].0;
    let tail: Vec<i32> = st
        .scan_docs_after_recordid("app", "c", Some(after_third))
        .unwrap()
        .iter()
        .map(|(_rid, b)| dec(b).get_i32("_id").unwrap())
        .collect();
    assert_eq!(
        tail,
        vec![20, 10],
        "tail must follow inserts, not _id order"
    );

    // min/max RecordId bound the collection (first/last inserted).
    let min = st.collection_min_recordid("app", "c").unwrap().unwrap();
    let max = st.collection_max_recordid("app", "c").unwrap().unwrap();
    assert!(min < max);
    assert_eq!(
        st.scan_docs_after_recordid("app", "c", Some(max))
            .unwrap()
            .len(),
        0,
        "nothing sorts after the max RecordId"
    );
    // A capped eviction removes the OLDEST-inserted (_id 50, the min RecordId);
    // after that, min RecordId advances past a cursor anchored at it → rollover.
    st.delete_by_id("app", "c", &Bson::Int32(50)).unwrap();
    let new_min = st.collection_min_recordid("app", "c").unwrap().unwrap();
    assert!(
        new_min > min,
        "min RecordId advances when the oldest is evicted"
    );

    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
