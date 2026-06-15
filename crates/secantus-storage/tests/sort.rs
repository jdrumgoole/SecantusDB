//! Sort-acceleration + hint tests (Phase 4 sub-phase 2, slice 2f): walking an
//! index to satisfy a sort (single-field + compound, forward/backward), the
//! COLLSCAN post-sort fallback, the multikey sort-exclusion, and `hint`
//! resolution ($natural / _id_ / by-name / by-keyspec / bad). Against real
//! WiredTiger.

use bson::{doc, Document};
use secantus_storage::{ExplainPlan, Hint, Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-sort-{}-{}", std::process::id(), n));
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

/// Result `_id`s in result order (no re-sort) for a filter+sort+hint query.
fn ids(st: &Storage, filter: Document, sort: Option<Document>, hint: Option<Hint>) -> Vec<i32> {
    st.find_matching_with(
        "app",
        "c",
        &filter,
        sort.as_ref(),
        hint.as_ref(),
        None,
        &bson::Document::new(),
    )
    .unwrap()
    .iter()
    .map(|b| id_of(b))
    .collect()
}

fn plan(st: &Storage, filter: Document, sort: Option<Document>, hint: Option<Hint>) -> ExplainPlan {
    st.explain_plan_with("app", "c", &filter, sort.as_ref(), hint.as_ref())
        .unwrap()
}

fn ixscan(name: &str, dir: &str) -> ExplainPlan {
    // key_pattern is checked separately where it matters; helper for direction.
    ExplainPlan::IxScan {
        index_name: name.to_string(),
        key_pattern: doc! {},
        direction: dir.to_string(),
    }
}

/// (index_name, direction) of a plan, or None for COLLSCAN.
fn plan_dir(p: &ExplainPlan) -> Option<(String, String)> {
    match p {
        ExplainPlan::IxScan {
            index_name,
            direction,
            ..
        } => Some((index_name.clone(), direction.clone())),
        ExplainPlan::CollScan => None,
    }
}

fn seed_a(st: &Storage) {
    st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 30, "b": 1}))
        .unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 10, "b": 3}))
        .unwrap();
    st.insert_one("app", "c", &enc(&doc! {"_id": 3, "a": 20, "b": 2}))
        .unwrap();
}

#[test]
fn single_field_sort_walks_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        seed_a(st);
        // Empty filter + sort {a:1}: walk a_1 forward -> a 10,20,30.
        assert_eq!(ids(st, doc! {}, Some(doc! {"a": 1}), None), vec![2, 3, 1]);
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": 1}), None)),
            Some(("a_1".into(), "forward".into()))
        );
        // Descending sort -> backward walk.
        assert_eq!(ids(st, doc! {}, Some(doc! {"a": -1}), None), vec![1, 3, 2]);
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": -1}), None)),
            Some(("a_1".into(), "backward".into()))
        );
    });
}

#[test]
fn descending_index_sort_direction() {
    with_db(|st| {
        st.create_index("app", "c", "a_d", &doc! {"a": -1}, &doc! {})
            .unwrap();
        seed_a(st);
        // sort {a:-1} matches a DESC index -> forward walk.
        assert_eq!(ids(st, doc! {}, Some(doc! {"a": -1}), None), vec![1, 3, 2]);
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": -1}), None)),
            Some(("a_d".into(), "forward".into()))
        );
        // sort {a:1} against a DESC index -> backward walk.
        assert_eq!(ids(st, doc! {}, Some(doc! {"a": 1}), None), vec![2, 3, 1]);
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": 1}), None)),
            Some(("a_d".into(), "backward".into()))
        );
    });
}

#[test]
fn sort_without_index_collscans_and_post_sorts() {
    with_db(|st| {
        seed_a(st); // no index
                    // sort {b:1}: COLLSCAN + post-sort -> b 1,2,3 -> ids 1,3,2.
        assert_eq!(ids(st, doc! {}, Some(doc! {"b": 1}), None), vec![1, 3, 2]);
        assert_eq!(
            plan(st, doc! {}, Some(doc! {"b": 1}), None),
            ExplainPlan::CollScan
        );
    });
}

#[test]
fn filter_on_sort_field_is_already_ordered() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        seed_a(st);
        // {a >= 20} sort {a:-1} -> {30,20} descending -> ids [1,3].
        assert_eq!(
            ids(st, doc! {"a": {"$gte": 20}}, Some(doc! {"a": -1}), None),
            vec![1, 3]
        );
    });
}

#[test]
fn compound_sort_exact_and_inverted() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 1, "b": 2}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 1, "b": 1}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "a": 2, "b": 5}))
            .unwrap();
        // {a:1,b:1} matches the index -> forward: (1,1)(1,2)(2,5) -> [2,1,3].
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": 1, "b": 1}), None),
            vec![2, 1, 3]
        );
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": 1, "b": 1}), None)),
            Some(("a_1_b_1".into(), "forward".into()))
        );
        // Full inverse {a:-1,b:-1} -> backward walk -> [3,1,2].
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": -1, "b": -1}), None),
            vec![3, 1, 2]
        );
        assert_eq!(
            plan_dir(&plan(st, doc! {}, Some(doc! {"a": -1, "b": -1}), None)),
            Some(("a_1_b_1".into(), "backward".into()))
        );
        // Mixed {a:1,b:-1} matches neither -> COLLSCAN + post-sort.
        assert_eq!(
            plan(st, doc! {}, Some(doc! {"a": 1, "b": -1}), None),
            ExplainPlan::CollScan
        );
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": 1, "b": -1}), None),
            vec![1, 2, 3]
        );
    });
}

#[test]
fn multikey_compound_sort_excluded() {
    with_db(|st| {
        st.create_index("app", "c", "a_1_b_1", &doc! {"a": 1, "b": 1}, &doc! {})
            .unwrap();
        // An array value flags the index multikey, disqualifying it for sort.
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": [1, 2], "b": 1}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 1, "b": 1}))
            .unwrap();
        assert_eq!(
            plan(st, doc! {}, Some(doc! {"a": 1, "b": 1}), None),
            ExplainPlan::CollScan
        );
    });
}

#[test]
fn hint_forces_index_and_natural() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        seed_a(st);
        // hint by name + sort on its field -> ordered IXSCAN.
        assert_eq!(
            ids(
                st,
                doc! {},
                Some(doc! {"a": 1}),
                Some(Hint::Name("a_1".into()))
            ),
            vec![2, 3, 1]
        );
        // hint by key spec resolves to the same index.
        assert_eq!(
            plan_dir(&plan(
                st,
                doc! {"a": 30},
                None,
                Some(Hint::KeySpec(doc! {"a": 1}))
            )),
            Some(("a_1".into(), "forward".into()))
        );
        // $natural forces a COLLSCAN even though a_1 would serve {a:30}.
        assert_eq!(
            plan(
                st,
                doc! {"a": 30},
                None,
                Some(Hint::Name("$natural".into()))
            ),
            ExplainPlan::CollScan
        );
        assert_eq!(
            ids(
                st,
                doc! {"a": 30},
                None,
                Some(Hint::Name("$natural".into()))
            ),
            vec![1]
        );
    });
}

#[test]
fn hint_id_index_orders_by_id() {
    with_db(|st| {
        seed_a(st);
        // _id_ hint with sort {_id:-1} -> reverse doc-table order.
        assert_eq!(
            ids(
                st,
                doc! {},
                Some(doc! {"_id": -1}),
                Some(Hint::Name("_id_".into()))
            ),
            vec![3, 2, 1]
        );
        assert_eq!(
            plan_dir(&plan(
                st,
                doc! {},
                Some(doc! {"_id": -1}),
                Some(Hint::Name("_id_".into()))
            )),
            Some(("_id_".into(), "backward".into()))
        );
    });
}

#[test]
fn bad_hint_errors_in_find_collscans_in_explain() {
    with_db(|st| {
        seed_a(st);
        let err = st
            .find_matching_with(
                "app",
                "c",
                &doc! {},
                None,
                Some(&Hint::Name("nope".into())),
                None,
                &bson::Document::new(),
            )
            .unwrap_err();
        assert!(matches!(err, StorageError::BadHint(_)));
        // explain degrades a bad hint to COLLSCAN.
        assert_eq!(
            plan(st, doc! {}, None, Some(Hint::Name("nope".into()))),
            ExplainPlan::CollScan
        );
    });
}

#[test]
fn explain_ixscan_key_pattern_shape() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        seed_a(st);
        // Sanity: the full IXSCAN carries the key pattern (helper above ignores it).
        assert_eq!(
            plan(st, doc! {}, Some(doc! {"a": 1}), None),
            ExplainPlan::IxScan {
                index_name: "a_1".into(),
                key_pattern: doc! {"a": 1},
                direction: "forward".into(),
            }
        );
        let _ = ixscan("a_1", "forward"); // keep helper referenced
    });
}
