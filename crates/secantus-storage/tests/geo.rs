//! 2d geohash index tests (Phase 4, slice geo-2): a `$geoWithin` query on a 2d
//! index scans the Z-order covering range; `find_matching` re-checks candidates
//! with `matches()` (so false positives from the coarse range are filtered).
//! Against real WiredTiger.

use bson::{doc, Bson, Document};
use secantus_storage::{ExplainPlan, Storage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-geo-{}-{}", std::process::id(), n));
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

fn pt(x: f64, y: f64) -> Bson {
    Bson::Array(vec![Bson::Double(x), Bson::Double(y)])
}

fn found_ids(st: &Storage, filter: Document) -> Vec<i32> {
    let mut v: Vec<i32> = st
        .find_matching("app", "c", &filter)
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

fn make_2d(st: &Storage) {
    st.create_index("app", "c", "loc_2d", &doc! {"loc": "2d"}, &doc! {})
        .unwrap();
}

#[test]
fn geo_within_box_uses_2d_index() {
    with_db(|st| {
        make_2d(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "loc": pt(5.0, 5.0)}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "loc": pt(50.0, 50.0)}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "loc": pt(1.0, 9.0)}))
            .unwrap();
        let q = doc! {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}};
        assert_eq!(found_ids(st, q.clone()), vec![1, 3]);
        assert!(matches!(
            st.explain_plan("app", "c", &q).unwrap(),
            ExplainPlan::IxScan { ref index_name, ref key_pattern, .. }
                if index_name == "loc_2d" && key_pattern == &doc! {"loc": "2d"}
        ));
    });
}

#[test]
fn geo_within_center_sphere() {
    with_db(|st| {
        make_2d(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "loc": pt(2.0, 2.0)}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "loc": pt(80.0, 80.0)}))
            .unwrap();
        // ~0.1 rad (~5.7 deg) cap at the origin: (2,2) is in, (80,80) is out.
        let q = doc! {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.1]}}};
        assert_eq!(found_ids(st, q), vec![1]);
    });
}

#[test]
fn two_d_index_is_point_only() {
    with_db(|st| {
        make_2d(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "loc": pt(5.0, 5.0)}))
            .unwrap();
        // A non-point (Polygon) geometry contributes no 2d entry.
        st.insert_one(
            "app",
            "c",
            &enc(&doc! {"_id": 2, "loc": {
                "type": "Polygon",
                "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]],
            }}),
        )
        .unwrap();
        // A doc with no loc field: also no entry.
        st.insert_one("app", "c", &enc(&doc! {"_id": 3, "x": 1}))
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "loc_2d").unwrap().len(), 1);
    });
}

#[test]
fn delete_and_replace_maintain_2d_entries() {
    with_db(|st| {
        make_2d(st);
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "loc": pt(5.0, 5.0)}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "loc": pt(6.0, 6.0)}))
            .unwrap();
        assert_eq!(st.index_entries("app", "c", "loc_2d").unwrap().len(), 2);
        // Replace moves the point out of a query box; delete removes its entry.
        st.replace_by_id(
            "app",
            "c",
            &Bson::Int32(1),
            &enc(&doc! {"loc": pt(99.0, 99.0)}),
        )
        .unwrap();
        st.delete_by_id("app", "c", &Bson::Int32(2)).unwrap();
        assert_eq!(st.index_entries("app", "c", "loc_2d").unwrap().len(), 1);
        // The moved point is no longer in the small box; nothing matches.
        let q = doc! {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}};
        assert_eq!(found_ids(st, q), Vec::<i32>::new());
    });
}

#[test]
fn create_2d_over_existing_data() {
    with_db(|st| {
        st.insert_one("app", "c", &enc(&doc! {"_id": 1, "loc": pt(3.0, 3.0)}))
            .unwrap();
        st.insert_one("app", "c", &enc(&doc! {"_id": 2, "loc": pt(40.0, 40.0)}))
            .unwrap();
        make_2d(st); // builds entries over the existing docs
        assert_eq!(st.index_entries("app", "c", "loc_2d").unwrap().len(), 2);
        let q = doc! {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}};
        assert_eq!(found_ids(st, q), vec![1]);
    });
}

#[test]
fn other_geo_index_types_still_rejected() {
    with_db(|st| {
        // 2dsphere / text / hashed are not supported yet.
        assert!(st
            .create_index("app", "c", "s", &doc! {"loc": "2dsphere"}, &doc! {})
            .is_err());
        assert!(st
            .create_index("app", "c", "t", &doc! {"name": "text"}, &doc! {})
            .is_err());
    });
}
