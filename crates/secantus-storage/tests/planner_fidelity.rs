//! Index choices that must not change the answer, and the two plans mongod
//! picks that this server used to miss. Every expected result here was
//! measured on mongod 8.2.11 (2026-09-30).
//!
//! Three of these were silent data loss on `main`: a sort that walked a
//! PARTIAL compound index under an empty filter, and a sort or hint that walked
//! a MULTIKEY index -- where a document whose field is an empty array has no
//! entry at all. `tools/probes/index_result_sets.py` now carries the same
//! shapes, curated and randomised.

use bson::{doc, Document};
use secantus_storage::{ExplainPlan, Hint, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn with_db(body: impl FnOnce(&Storage)) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home: PathBuf =
        std::env::temp_dir().join(format!("secantus-planner-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn insert(st: &Storage, docs: &[Document]) {
    for d in docs {
        st.insert_one("app", "c", &bson::to_vec(d).unwrap())
            .unwrap();
    }
}

fn ids(st: &Storage, filter: Document, sort: Option<Document>, hint: Option<Hint>) -> Vec<i32> {
    st.find_matching_with(
        "app",
        "c",
        &filter,
        sort.as_ref(),
        hint.as_ref(),
        None,
        &Document::new(),
    )
    .unwrap()
    .iter()
    .map(|b| {
        Document::from_reader(&mut std::io::Cursor::new(b))
            .unwrap()
            .get_i32("_id")
            .unwrap()
    })
    .collect()
}

fn plan_index(st: &Storage, filter: Document, sort: Option<Document>) -> Option<String> {
    match st
        .explain_plan_with("app", "c", &filter, sort.as_ref(), None)
        .unwrap()
    {
        ExplainPlan::IxScan { index_name, .. } => Some(index_name),
        ExplainPlan::CollScan | ExplainPlan::Or { .. } => None,
    }
}

/// `[2, 4, 3, 1, 5]` ascending: the empty array sorts below a missing field,
/// then `[5, 1]` by its smallest element.
fn seed_empty_array(st: &Storage) {
    insert(
        st,
        &[
            doc! {"_id": 1, "a": 2},
            doc! {"_id": 2, "a": []},
            doc! {"_id": 3, "a": [5, 1]},
            doc! {"_id": 4},
            doc! {"_id": 5, "a": 3},
        ],
    );
}

#[test]
fn compound_sort_does_not_walk_a_partial_index_it_cannot_use() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "ab_part",
            &doc! {"a": 1, "b": 1},
            &doc! {"partialFilterExpression": {"a": {"$gt": 5}}},
        )
        .unwrap();
        let docs: Vec<Document> = (0..10)
            .map(|i| doc! {"_id": i, "a": i, "b": i % 3})
            .collect();
        insert(st, &docs);
        // mongod: all 10. This returned the 4 the partial index holds.
        let asc = ids(st, doc! {}, Some(doc! {"a": 1, "b": 1}), None);
        assert_eq!(asc, (0..10).collect::<Vec<_>>());
        let desc = ids(st, doc! {}, Some(doc! {"a": -1, "b": -1}), None);
        assert_eq!(desc, (0..10).rev().collect::<Vec<_>>());
        assert_eq!(plan_index(st, doc! {}, Some(doc! {"a": 1, "b": 1})), None);
        // A filter that implies the partial filter may still walk it.
        assert_eq!(
            ids(
                st,
                doc! {"a": {"$gt": 7}},
                Some(doc! {"a": 1, "b": 1}),
                None
            ),
            vec![8, 9]
        );
    });
}

#[test]
fn sort_does_not_walk_a_multikey_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        seed_empty_array(st);
        // mongod: the empty-array document is FIRST. It was missing.
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": 1}), None),
            vec![2, 4, 3, 1, 5]
        );
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": -1}), None),
            vec![3, 5, 1, 4, 2]
        );
        assert_eq!(
            ids(st, doc! {"a": {"$gte": 1}}, Some(doc! {"a": -1}), None),
            vec![3, 5, 1]
        );
    });
}

#[test]
fn hinted_multikey_index_keeps_empty_array_documents() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        st.create_index("app", "c", "a_sp", &doc! {"a": 1}, &doc! {"sparse": true})
            .unwrap();
        seed_empty_array(st);
        let h = |n: &str| Some(Hint::Name(n.to_string()));
        // mongod, in index order.
        assert_eq!(ids(st, doc! {}, None, h("a_1")), vec![2, 4, 3, 1, 5]);
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"a": -1}), h("a_1")),
            vec![3, 5, 1, 4, 2]
        );
        // Sparse: an empty array is PRESENT, so it is indexed; a missing field
        // is not.
        assert_eq!(ids(st, doc! {}, None, h("a_sp")), vec![2, 3, 1, 5]);
    });
}

#[test]
fn multi_field_filter_rides_one_single_field_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        st.create_index("app", "c", "b_1", &doc! {"b": 1}, &doc! {})
            .unwrap();
        let docs: Vec<Document> = (0..20)
            .map(|i| doc! {"_id": i, "a": i % 4, "b": (i % 5).to_string()})
            .collect();
        insert(st, &docs);
        // mongod picks an IXSCAN and re-checks the other field in the FETCH.
        assert!(plan_index(st, doc! {"a": 1, "b": "1"}, None).is_some());
        assert_eq!(ids(st, doc! {"a": 1, "b": "1"}, None, None), vec![1]);
        let expected: Vec<i32> = (0..20).filter(|i| i % 4 == 1 && i % 5 != 1).collect();
        assert_eq!(
            ids(
                st,
                doc! {"a": 1, "b": {"$ne": "1"}},
                Some(doc! {"_id": 1}),
                None
            ),
            expected
        );
    });
}

#[test]
fn sort_under_an_unindexed_filter_walks_the_sort_index() {
    with_db(|st| {
        st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {})
            .unwrap();
        let docs: Vec<Document> = (0..10)
            .map(|i| doc! {"_id": i, "a": (i * 7) % 10, "nope": i % 2})
            .collect();
        insert(st, &docs);
        assert_eq!(
            plan_index(st, doc! {"nope": 1}, Some(doc! {"a": -1})),
            Some("a_1".to_string())
        );
        // a = 7, 1, 5, 9, 3 for the odd ids 1, 3, 5, 7, 9.
        assert_eq!(
            ids(st, doc! {"nope": 1}, Some(doc! {"a": -1}), None),
            vec![7, 1, 5, 9, 3]
        );
    });
}

#[test]
fn sort_under_a_filter_skips_a_sparse_index_the_filter_cannot_cover() {
    with_db(|st| {
        st.create_index("app", "c", "a_sp", &doc! {"a": 1}, &doc! {"sparse": true})
            .unwrap();
        insert(
            st,
            &[
                doc! {"_id": 1, "a": 2, "k": 1},
                doc! {"_id": 2, "k": 1},
                doc! {"_id": 3, "a": 1, "k": 1},
            ],
        );
        // `k: 1` says nothing about `a`, so the sparse index would drop _id 2.
        assert_eq!(plan_index(st, doc! {"k": 1}, Some(doc! {"a": 1})), None);
        assert_eq!(
            ids(st, doc! {"k": 1}, Some(doc! {"a": 1}), None),
            vec![2, 3, 1]
        );
    });
}

/// The sort key's byte form ranks a document or array by its raw BSON, which
/// starts with its LENGTH. mongod compares field by field / element by element
/// (`{a: 2, b: 1, c: ...} < {a: 2, b: [3]} < {a: 3} < {a: 5} < {a: 9, ...}`),
/// with or without an index on the field.
#[test]
fn embedded_documents_sort_by_value_not_by_encoded_length() {
    for indexed in [false, true] {
        with_db(|st| {
            if indexed {
                st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
                    .unwrap();
            }
            insert(
                st,
                &[
                    doc! {"_id": 1, "x": {"a": 5}},
                    doc! {"_id": 2, "x": {"a": 2, "b": [3]}},
                    doc! {"_id": 3, "x": {"a": 3}},
                    doc! {"_id": 4, "x": {"a": 2, "b": 1, "c": "long string here"}},
                    doc! {"_id": 5, "x": {"a": 9, "z": {"q": [1, 2, 3, 4, 5, 6]}}},
                ],
            );
            assert_eq!(
                ids(st, doc! {}, Some(doc! {"x": 1}), None),
                vec![4, 2, 3, 1, 5],
                "indexed={indexed}"
            );
            assert_eq!(
                ids(
                    st,
                    doc! {"x": {"$gt": {"a": 0}}},
                    Some(doc! {"x": -1}),
                    None
                ),
                vec![5, 1, 3, 2, 4],
                "indexed={indexed}"
            );
        });
    }
}

#[test]
fn nested_arrays_sort_element_by_element() {
    with_db(|st| {
        insert(
            st,
            &[
                doc! {"_id": 1, "x": [[5]]},
                doc! {"_id": 2, "x": [1, [2, [3]]]},
                doc! {"_id": 3, "x": [[1], [9]]},
                doc! {"_id": 4, "x": []},
                doc! {"_id": 5, "x": [[]]},
            ],
        );
        // mongod: descending by each array's largest element, compared
        // element-wise -- [9] > [5] > [2, [3]] > [] ; the empty array last.
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"x": -1}), None),
            vec![3, 1, 2, 5, 4]
        );
        assert_eq!(
            ids(st, doc! {}, Some(doc! {"x": 1}), None),
            vec![4, 2, 5, 3, 1]
        );
    });
}

/// A hinted walk returns the index's ORDER, and for document keys that is value
/// order, not encoded-length order (mongod 8.2.11: `[4, 2, 3, 1, 5]`).
#[test]
fn hinted_walk_over_document_keys_is_in_value_order() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        st.create_index("app", "c", "x_d", &doc! {"x": -1}, &doc! {})
            .unwrap();
        insert(
            st,
            &[
                doc! {"_id": 1, "x": {"a": 5}},
                doc! {"_id": 2, "x": {"a": 2, "b": [3]}},
                doc! {"_id": 3, "x": {"a": 3}},
                doc! {"_id": 4, "x": {"a": 2, "b": 1, "c": "long string here"}},
                doc! {"_id": 5, "x": {"a": 9, "z": {"q": [1, 2, 3, 4, 5, 6]}}},
            ],
        );
        let h = |n: &str| Some(Hint::Name(n.to_string()));
        assert_eq!(ids(st, doc! {}, None, h("x_1")), vec![4, 2, 3, 1, 5]);
        assert_eq!(ids(st, doc! {}, None, h("x_d")), vec![5, 1, 3, 2, 4]);
    });
}

/// An index range scan bounded by an array or a document compared the
/// entries' raw-BSON bytes, which order by LENGTH first: `{x: {$gt: [1, 2, 3]}}`
/// dropped `{x: [9]}`. Such bounds no longer use the index; equality still does.
#[test]
fn array_and_document_range_bounds_do_not_scan_index_bytes() {
    with_db(|st| {
        st.create_index("app", "c", "x_1", &doc! {"x": 1}, &doc! {})
            .unwrap();
        insert(
            st,
            &[
                doc! {"_id": 6, "x": [1, 2]},
                doc! {"_id": 7, "x": [9]},
                doc! {"_id": 8, "x": [1, 2, 3, 4, 5, 6, 7, 8]},
                doc! {"_id": 1, "x": {"a": 5}},
                doc! {"_id": 4, "x": {"a": 2, "b": 1, "c": "long string here"}},
            ],
        );
        let sorted = |mut v: Vec<i32>| {
            v.sort();
            v
        };
        // mongod 8.2.11.
        assert_eq!(
            sorted(ids(st, doc! {"x": {"$gt": [1, 2, 3]}}, None, None)),
            vec![7, 8]
        );
        assert_eq!(
            sorted(ids(
                st,
                doc! {"x": {"$gte": [1, 2, 3, 4, 5, 6, 7, 8, 9]}},
                None,
                None
            )),
            vec![7]
        );
        assert_eq!(
            sorted(ids(
                st,
                doc! {"x": {"$lt": {"a": 4, "long": "xxxxxxxx"}}},
                None,
                None
            )),
            vec![4]
        );
        assert_eq!(ids(st, doc! {"x": [9]}, None, None), vec![7]);
    });
}
