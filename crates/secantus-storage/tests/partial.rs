//! Partial-index query-implication tests (Phase 4 sub-phase 2, slice 2e-2): a
//! partial index accelerates a query only when the query implies its
//! `partialFilterExpression`; otherwise the lookup falls back to COLLSCAN (but
//! still returns correct results). Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::{ExplainPlan, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-partial-{}-{}", std::process::id(), n));
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

fn found_ids(st: &Storage, db: &str, coll: &str, filter: Document) -> Vec<i32> {
    let mut v: Vec<i32> = st
        .find_matching(db, coll, &filter)
        .unwrap()
        .iter()
        .map(|b| {
            Document::from_reader(&mut std::io::Cursor::new(b.as_slice()))
                .unwrap()
                .get_i32("_id")
                .unwrap()
        })
        .collect();
    v.sort();
    v
}

fn is_ixscan(p: &ExplainPlan, name: &str) -> bool {
    matches!(p, ExplainPlan::IxScan { index_name, .. } if index_name == name)
}

#[test]
fn single_field_partial_used_only_when_implied() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "n_1",
            &doc! {"n": 1},
            &doc! {"partialFilterExpression": {"status": "active"}},
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 1, "n": 5, "status": "active"}),
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "n": 5, "status": "inactive"}),
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 3, "n": 7, "status": "active"}),
        )
        .unwrap();

        // Query implies the partial filter -> IXSCAN, only the active n=5 doc.
        let q = doc! {"status": "active", "n": 5};
        assert!(is_ixscan(&st.explain_plan("app", "c", &q).unwrap(), "n_1"));
        assert_eq!(found_ids(st, "app", "c", q), vec![1]);

        // Query without the implication -> COLLSCAN, but correct results (the
        // inactive n=5 doc is a legitimate match for a status-free query).
        let q2 = doc! {"n": 5};
        assert_eq!(
            st.explain_plan("app", "c", &q2).unwrap(),
            ExplainPlan::CollScan
        );
        assert_eq!(found_ids(st, "app", "c", q2), vec![1, 2]);
    });
}

#[test]
fn compound_partial_strips_filter_key_and_accelerates() {
    with_db(|st| {
        st.create_index(
            "app",
            "c",
            "a_1_b_1",
            &doc! {"a": 1, "b": 1},
            &doc! {"partialFilterExpression": {"tenant": "t1"}},
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 1, "tenant": "t1", "a": 5, "b": 1}),
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "tenant": "t2", "a": 5, "b": 1}),
        )
        .unwrap();
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 3, "tenant": "t1", "a": 9, "b": 1}),
        )
        .unwrap();

        // {tenant:"t1", a:5}: tenant is stripped (guaranteed by the index), the
        // remaining {a} is a leading prefix of {a,b} -> IXSCAN.
        let q = doc! {"tenant": "t1", "a": 5};
        assert!(is_ixscan(
            &st.explain_plan("app", "c", &q).unwrap(),
            "a_1_b_1"
        ));
        assert_eq!(found_ids(st, "app", "c", q), vec![1]);

        // {a:5} alone doesn't imply tenant:"t1" -> COLLSCAN, both a=5 docs.
        let q2 = doc! {"a": 5};
        assert_eq!(
            st.explain_plan("app", "c", &q2).unwrap(),
            ExplainPlan::CollScan
        );
        assert_eq!(found_ids(st, "app", "c", q2), vec![1, 2]);
    });
}
