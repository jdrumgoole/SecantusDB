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
