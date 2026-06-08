//! Compound-index query tests (Phase 4 sub-phase 2, slice 2c): bare-equality
//! prefix (full cover + leading prefix + field-order independence), prefix +
//! trailing operator ($eq / $in / range), mixed-direction compound indexes, and
//! `explain_plan`, against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::{ExplainPlan, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-cmp-{}-{}", std::process::id(), n));
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

fn id_of(blob: &[u8]) -> i32 {
    Document::from_reader(&mut std::io::Cursor::new(blob))
        .unwrap()
        .get_i32("_id")
        .unwrap()
}

fn found_ids(st: &Storage, db: &str, coll: &str, filter: Document) -> Vec<i32> {
    st.find_matching(db, coll, &filter)
        .unwrap()
        .iter()
        .map(|b| id_of(b))
        .collect()
}

fn sorted(mut v: Vec<i32>) -> Vec<i32> {
    v.sort();
    v
}

/// Four docs over (a, b): the 2x2 grid a∈{1,2} × b∈{10,20}.
fn seed_ab(st: &Storage) {
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1, "b": 10}))
        .unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 1, "b": 20}))
        .unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 3, "a": 2, "b": 10}))
        .unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 4, "a": 2, "b": 20}))
        .unwrap();
}

fn is_ixscan(p: &ExplainPlan, name: &str) -> bool {
    matches!(p, ExplainPlan::IxScan { index_name, .. } if index_name == name)
}

#[test]
fn compound_full_match_and_prefix() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        seed_ab(st);
        // Full match.
        assert_eq!(found_ids(st, "app", "c", doc! {"a": 1, "b": 10}), vec![1]);
        // Leading prefix: a=1 -> sorted by b asc -> [1 (b10), 2 (b20)].
        assert_eq!(found_ids(st, "app", "c", doc! {"a": 1}), vec![1, 2]);
        // Filter field order doesn't matter.
        assert_eq!(found_ids(st, "app", "c", doc! {"b": 10, "a": 1}), vec![1]);
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1, "b": 10})
                .unwrap(),
            "a_1_b_1"
        ));
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1}).unwrap(),
            "a_1_b_1"
        ));
    });
}

#[test]
fn compound_prefix_trailing_operator() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        seed_ab(st);
        // eq + range.
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 1, "b": {"$gt": 10}}),
            vec![2]
        );
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 2, "b": {"$lte": 20}}),
            vec![3, 4]
        );
        // eq + $eq.
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 1, "b": {"$eq": 20}}),
            vec![2]
        );
        // eq + $in.
        assert_eq!(
            sorted(found_ids(
                st,
                "app",
                "c",
                doc! {"a": 2, "b": {"$in": [10, 20]}}
            )),
            vec![3, 4]
        );
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1, "b": {"$gt": 10}})
                .unwrap(),
            "a_1_b_1"
        ));
        // The range must not leak across the leading-equality prefix: a=1,b>5
        // returns only a=1 docs, never a=2.
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"a": 1, "b": {"$gt": 5}})),
            vec![1, 2]
        );
    });
}

#[test]
fn compound_prefers_shorter_index_and_three_field() {
    with_db(|st| {
        // Both a single-field {a} index and a compound {a,b} exist; a bare {a}
        // equality prefers the shorter single-field index.
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        seed_ab(st);
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1}).unwrap(),
            "a_1"
        ));
        // {a, b} bare-eq still uses the compound index (single-field can't cover b).
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1, "b": 10})
                .unwrap(),
            "a_1_b_1"
        ));
    });
}

#[test]
fn three_field_compound() {
    with_db(|st| {
        st.create_index("app", "c", "abc", &doc! {"a": 1, "b": 1, "c": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1, "b": 2, "c": 3}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 1, "b": 2, "c": 9}))
            .unwrap();
        // Two-field prefix.
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"a": 1, "b": 2})),
            vec![1, 2]
        );
        // Full three-field match.
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 1, "b": 2, "c": 3}),
            vec![1]
        );
        // Two equalities + trailing range on the third column.
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 1, "b": 2, "c": {"$gte": 9}}),
            vec![2]
        );
    });
}

#[test]
fn mixed_direction_compound() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_-1", &doc! {"a": 1, "b": -1}, &doc! {})
            .unwrap();
        seed_ab(st);
        // Full match still works.
        assert_eq!(found_ids(st, "app", "c", doc! {"a": 1, "b": 10}), vec![1]);
        // Leading prefix a=1: walked in the index's natural order (b DESC) ->
        // [2 (b20), 1 (b10)].
        assert_eq!(found_ids(st, "app", "c", doc! {"a": 1}), vec![2, 1]);
        // Trailing range on the DESC field: a=1, b>10 -> b=20 -> [2].
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 1, "b": {"$gt": 10}}),
            vec![2]
        );
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": 2, "b": {"$lt": 20}}),
            vec![3]
        );
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &doc! {"a": 1, "b": {"$gt": 10}})
                .unwrap(),
            "a_1_b_-1"
        ));
    });
}

#[test]
fn no_covering_compound_index_is_collscan() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        seed_ab(st);
        // A filter on a non-leading field alone can't use the compound index.
        assert_eq!(
            st.explain_plan("app", "c", &doc! {"b": 10}).unwrap(),
            ExplainPlan::CollScan
        );
        // ...but still returns correct results via COLLSCAN.
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"b": 10})),
            vec![1, 3]
        );
    });
}
