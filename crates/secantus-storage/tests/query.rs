//! Query-routing integration tests (Phase 4 sub-phase 2, slice 2b): single-field
//! index lookups (equality / `$in` / range), the `_id` point-lookup fast path,
//! COLLSCAN fallback, and `explain_plan`, against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::{ExplainPlan, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-q-{}-{}", std::process::id(), n));
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

/// `_id`s of `find_matching` results, in result order.
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

#[test]
fn equality_uses_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        for (i, a) in [(1, 10), (2, 20), (3, 10)] {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": a}))
                .unwrap();
        }
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"a": 10})),
            vec![1, 3]
        );
        assert!(matches!(
            st.explain_plan("app", "c", &doc! {"a": 10}).unwrap(),
            ExplainPlan::IxScan { ref index_name, .. } if index_name == "a_1"
        ));
    });
}

#[test]
fn collscan_when_no_index() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "a": 10}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "a": 20}))
            .unwrap();
        assert_eq!(found_ids(st, "app", "c", doc! {"a": 20}), vec![2]);
        assert_eq!(
            st.explain_plan("app", "c", &doc! {"a": 20}).unwrap(),
            ExplainPlan::CollScan
        );
    });
}

#[test]
fn in_operator_uses_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        for i in 1..=4 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": i * 10}))
                .unwrap();
        }
        assert_eq!(
            sorted(found_ids(
                st,
                "app",
                "c",
                doc! {"a": {"$in": [10, 30, 999]}}
            )),
            vec![1, 3]
        );
    });
}

#[test]
fn range_scan_ascending_order() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": i}))
                .unwrap();
        }
        // Index walk yields entries in ascending `a` order (== _id here).
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": {"$gt": 2, "$lte": 4}}),
            vec![3, 4]
        );
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": {"$gte": 4}}),
            vec![4, 5]
        );
        assert_eq!(found_ids(st, "app", "c", doc! {"a": {"$lt": 2}}), vec![1]);
    });
}

#[test]
fn descending_index_range_flips() {
    with_db(|st| {
        st.create_index("app", "c", "a_d", &doc! {"a": -1}, &doc! {})
            .unwrap();
        for i in 1..=5 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": i}))
                .unwrap();
        }
        // `a >= 4` -> {4, 5}; a DESC index walks descending, so result order is
        // a=5 (id 5) then a=4 (id 4).
        assert_eq!(
            found_ids(st, "app", "c", doc! {"a": {"$gte": 4}}),
            vec![5, 4]
        );
        assert!(matches!(
            st.explain_plan("app", "c", &doc! {"a": {"$lt": 3}}).unwrap(),
            ExplainPlan::IxScan { ref index_name, .. } if index_name == "a_d"
        ));
    });
}

#[test]
fn id_point_lookup() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i, "a": i}))
                .unwrap();
        }
        assert_eq!(found_ids(st, "app", "c", doc! {"_id": 2}), vec![2]);
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"_id": {"$in": [1, 3]}})),
            vec![1, 3]
        );
        // explain reports the virtual `_id_` index.
        match st.explain_plan("app", "c", &doc! {"_id": 2}).unwrap() {
            ExplainPlan::IxScan {
                index_name,
                key_pattern,
                ..
            } => {
                assert_eq!(index_name, "_id_");
                assert_eq!(key_pattern, doc! {"_id": 1});
            }
            other => panic!("expected IXSCAN, got {other:?}"),
        }
        // A range on `_id` has no doc-table range scan in 2b -> COLLSCAN (correct
        // results via matches()).
        assert_eq!(
            st.explain_plan("app", "c", &doc! {"_id": {"$gt": 1}})
                .unwrap(),
            ExplainPlan::CollScan
        );
        assert_eq!(
            sorted(found_ids(st, "app", "c", doc! {"_id": {"$gt": 1}})),
            vec![2, 3]
        );
    });
}

#[test]
fn multikey_scalar_element_query_via_index() {
    with_db(|st| {
        st.create_index("app", "c", "tags_1", &doc! {"tags": 1}, &doc! {})
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "tags": ["py", "go"]}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "tags": ["rust"]}))
            .unwrap();
        // Scalar element match lands on the per-element entry; matches() confirms
        // the candidate (array-contains-scalar).
        assert_eq!(found_ids(st, "app", "c", doc! {"tags": "py"}), vec![1]);
        assert_eq!(found_ids(st, "app", "c", doc! {"tags": "rust"}), vec![2]);
        // (Whole-array *literal* equality `{tags: [array]}` is a `matches()`
        // Fallback in the Rust query engine — surfaced as QueryUnsupported and
        // routed to Python by the server's engine selection, so it isn't
        // exercised at this layer.)
    });
}

#[test]
fn empty_filter_returns_all_via_collscan() {
    with_db(|st| {
        for i in 1..=3 {
            st.insert_one("app", "c", &enc(&doc! {"_id": i})).unwrap();
        }
        assert_eq!(found_ids(st, "app", "c", doc! {}).len(), 3);
        assert_eq!(
            st.explain_plan("app", "c", &doc! {}).unwrap(),
            ExplainPlan::CollScan
        );
    });
}
