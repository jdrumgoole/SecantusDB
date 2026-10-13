//! The geo queries over real WiredTiger: which documents `$geoWithin`,
//! `$geoIntersects`, `$near` and `$geoNear` return, in what order, and what
//! a geo index refuses to hold. Every expectation is what mongod 8.2.11
//! answered to the same command over the same documents
//! (`tools/probes/geo_queries.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn point(x: f64, y: f64) -> Document {
    doc! { "type": "Point", "coordinates": [x, y] }
}

fn square() -> Document {
    doc! { "type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]]] }
}

/// GeoJSON and legacy shapes together, as a `2dsphere` index takes them.
fn sphere() -> Vec<Document> {
    vec![
        doc! { "_id": 4, "loc": point(1.0, 1.0) },
        doc! { "_id": 1, "loc": point(0.0, 0.0) },
        doc! { "_id": 9, "loc": point(10.0, 10.0) },
        doc! { "_id": 2, "loc": point(2.5, 2.5) },
        doc! { "_id": 7, "loc": point(-73.97, 40.77) },
        doc! { "_id": 3, "loc": point(-73.88, 40.78) },
        doc! { "_id": 12, "loc": point(179.9, 0.0) },
        doc! { "_id": 5, "loc": point(-179.9, 0.0) },
        doc! { "_id": 8, "loc": point(0.0, 89.9) },
        doc! { "_id": 6, "loc": { "type": "LineString", "coordinates": [[0, 0], [2, 2]] } },
        doc! { "_id": 11, "loc": square() },
        doc! { "_id": 10, "loc": { "type": "Polygon", "coordinates": [[[5, 5], [6, 5], [6, 6], [5, 6], [5, 5]]] } },
        doc! { "_id": 13, "loc": [1.5, 1.5] },
        doc! { "_id": 14, "loc": { "lng": 2, "lat": 2 } },
        doc! { "_id": 15, "k": "a" },
        doc! { "_id": 16, "loc": Bson::Null },
        doc! { "_id": 17, "loc": { "type": "MultiPoint", "coordinates": [[1, 1], [30, 30]] } },
        doc! { "_id": 18, "loc": point(3.0, 3.0) },
        doc! { "_id": 19, "loc": point(0.0, 3.0) },
        doc! { "_id": 20, "loc": point(1.0, 1.0) },
    ]
}

fn seed(c: &mut CommandContext, docs: Vec<Document>, kind: Option<&str>) {
    dispatch_full(&doc! { "drop": "c" }, c);
    let reply = dispatch_full(&doc! { "insert": "c", "documents": docs }, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    assert!(!reply.contains_key("writeErrors"), "{reply}");
    if let Some(kind) = kind {
        let reply = dispatch_full(
            &doc! { "createIndexes": "c", "indexes": [{ "key": { "loc": kind }, "name": "geo" }] },
            c,
        );
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    }
}

fn batch(reply: &Document) -> Vec<Document> {
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn ids(reply: &Document) -> Vec<i32> {
    batch(reply)
        .iter()
        .map(|d| d.get_i32("_id").unwrap())
        .collect()
}

fn sorted(ids: &[i32]) -> Vec<i32> {
    let mut out = ids.to_vec();
    out.sort();
    out
}

fn find(c: &mut CommandContext, filter: Document) -> Document {
    dispatch_full(&doc! { "find": "c", "filter": filter }, c)
}

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

fn write_error(reply: &Document) -> (i32, String) {
    let first = reply.get_array("writeErrors").expect("a write error")[0]
        .as_document()
        .unwrap()
        .clone();
    (
        first.get_i32("code").unwrap(),
        first.get_str("errmsg").unwrap().to_string(),
    )
}

#[test]
fn a_flat_shape_holds_points_and_its_own_edge() {
    with_wt(|c| {
        seed(c, sphere(), None);
        // The line and the two polygons are inside the box and are not
        // returned: a flat shape holds points.
        let in_box = doc! { "loc": { "$geoWithin": { "$box": [[0, 0], [3, 3]] } } };
        assert_eq!(ids(&find(c, in_box)), [4, 1, 2, 13, 14, 18, 19, 20]);
        let old_name = doc! { "loc": { "$within": { "$box": [[0, 0], [3, 3]] } } };
        assert_eq!(ids(&find(c, old_name)), [4, 1, 2, 13, 14, 18, 19, 20]);
        // Several locations in one value: any of them.
        seed(
            c,
            vec![
                doc! { "_id": 1, "loc": [[1, 1], [30, 30]] },
                doc! { "_id": 2, "loc": [3, 0, 99] },
                doc! { "_id": 3, "loc": [9, 9] },
            ],
            None,
        );
        let in_box = doc! { "loc": { "$geoWithin": { "$box": [[0, 0], [3, 3]] } } };
        assert_eq!(ids(&find(c, in_box)), [1, 2]);
    });
}

#[test]
fn a_polygon_is_tested_on_the_sphere() {
    with_wt(|c| {
        seed(c, sphere(), None);
        let holed = doc! { "type": "Polygon", "coordinates": [
            [[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]],
            [[1, 1], [2, 1], [2, 2], [1, 2], [1, 1]],
        ] };
        // `[1.5, 1.5]` is in the hole; the square polygon has the hole in it.
        assert_eq!(
            ids(&find(
                c,
                doc! { "loc": { "$geoWithin": { "$geometry": holed } } }
            )),
            [4, 1, 2, 14, 18, 19, 20]
        );
        let across = doc! { "type": "Polygon", "coordinates": [
            [[179, -1], [-179, -1], [-179, 1], [179, 1], [179, -1]],
        ] };
        assert_eq!(
            ids(&find(
                c,
                doc! { "loc": { "$geoWithin": { "$geometry": across } } }
            )),
            [12, 5]
        );
    });
}

#[test]
fn an_edge_is_the_great_circle_between_its_ends() {
    with_wt(|c| {
        seed(c, sphere(), None);
        // `[1, 1]` is not on the line from `[0, 0]` to `[2, 2]`: document 6
        // is not returned.
        let at = |g: Document| doc! { "loc": { "$geoIntersects": { "$geometry": g } } };
        assert_eq!(ids(&find(c, at(point(1.0, 1.0)))), [4, 11, 17, 20]);
        let diagonal = doc! { "type": "LineString", "coordinates": [[0, 0], [10, 10]] };
        assert_eq!(ids(&find(c, at(diagonal))), [1, 9, 6, 11, 10]);
    });
}

#[test]
fn the_refusals_are_mongods() {
    with_wt(|c| {
        seed(c, sphere(), None);
        let (code, message) = refusal(&find(
            c,
            doc! { "loc": { "$geoWithin": { "$geometry": point(1.0, 1.0) } } },
        ));
        assert_eq!(code, 2);
        assert!(
            message.starts_with("$within not supported with provided geometry"),
            "{message}"
        );
        let (_, message) = refusal(&find(
            c,
            doc! { "loc": { "$geoWithin": { "$box": [[0, 0]] } } },
        ));
        assert_eq!(
            message,
            "Point must be an array or object, instead got type missing"
        );
        let (_, message) = refusal(&find(
            c,
            doc! { "loc": { "$geoIntersects": { "$box": [[0, 0], [3, 3]] } } },
        ));
        assert!(
            message.starts_with("$geoIntersect not supported with provided geometry"),
            "{message}"
        );
    });
}

#[test]
fn near_needs_its_index_and_returns_nearest_first() {
    with_wt(|c| {
        let near = |extra: Document| {
            let mut spec = doc! { "$geometry": point(0.0, 0.0) };
            spec.extend(extra);
            doc! { "loc": { "$near": spec } }
        };
        seed(c, sphere(), None);
        let (code, message) = refusal(&find(c, near(doc! {})));
        assert_eq!(code, 291);
        assert!(
            message.ends_with("unable to find index for $geoNear query"),
            "{message}"
        );

        seed(c, sphere(), Some("2dsphere"));
        // The polygon holds the centre, and the line starts at it but is
        // never at no distance.
        // Documents 4 and 20 are one point: mongod returns them in the order
        // its index scan meets them, which is not pinned here.
        let got = ids(&find(c, near(doc! { "$maxDistance": 200000 })));
        assert_eq!(got[..3], [1, 11, 6]);
        assert_eq!(sorted(&got[3..]), [4, 17, 20]);
        assert_eq!(ids(&find(c, near(doc! { "$maxDistance": 0 }))), [1, 11]);
        // A legacy `$near` reads a `2d` index, and this is not one.
        assert_eq!(
            refusal(&find(c, doc! { "loc": { "$near": [0, 0] } })).0,
            291
        );
        let counted = dispatch_full(&doc! { "count": "c", "query": near(doc! {}) }, c);
        assert_eq!(counted.get_i32("n"), Ok(18), "{counted}");
        let (_, message) = refusal(&find(c, doc! { "$or": [near(doc! {}), { "_id": 1 }] }));
        assert_eq!(message, "geo $near must be top-level expr");
        let matched = dispatch_full(
            &doc! { "aggregate": "c", "pipeline": [{ "$match": near(doc! {}) }], "cursor": {} },
            c,
        );
        assert_eq!(refusal(&matched).0, 5626500);
    });
}

#[test]
fn geo_near_measures_every_geometry() {
    with_wt(|c| {
        seed(c, sphere(), Some("2dsphere"));
        let stage = |spec: Document| {
            doc! { "aggregate": "c", "pipeline": [{ "$geoNear": spec }], "cursor": {} }
        };
        let reply = dispatch_full(
            &stage(doc! { "near": point(0.0, 0.0), "distanceField": "d", "maxDistance": 200000 }),
            c,
        );
        assert_eq!(ids(&reply)[..3], [1, 11, 6]);
        assert_eq!(sorted(&ids(&reply)[3..]), [4, 17, 20]);
        let distances: Vec<f64> = batch(&reply)
            .iter()
            .map(|d| d.get_f64("d").unwrap())
            .collect();
        assert_eq!(distances[0], 0.0);
        assert!(
            (distances[2] - 6.42567661e-12).abs() < 1e-18,
            "{distances:?}"
        );
        assert!((distances[3] - 157_424.624).abs() < 0.001, "{distances:?}");
        // A legacy point on the sphere measures in radians.
        let reply = dispatch_full(
            &stage(
                doc! { "near": [0, 0], "distanceField": "d", "spherical": true, "maxDistance": 0.05 },
            ),
            c,
        );
        assert_eq!(ids(&reply)[..3], [1, 11, 6]);
        assert_eq!(sorted(&ids(&reply)[3..6]), [4, 17, 20]);
        assert_eq!(ids(&reply)[6..], [13, 14]);
        let reply = dispatch_full(
            &stage(doc! { "near": point(0.0, 0.0), "distanceField": "d", "limit": 2 }),
            c,
        );
        assert_eq!(refusal(&reply).0, 50858);
        let later = dispatch_full(
            &doc! { "aggregate": "c", "cursor": {}, "pipeline": [
                { "$match": { "k": "a" } },
                { "$geoNear": { "near": point(0.0, 0.0), "distanceField": "d" } },
            ] },
            c,
        );
        assert_eq!(refusal(&later).0, 40603);
    });
}

#[test]
fn a_geo_index_refuses_what_it_cannot_hold() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": 0 }], Some("2dsphere"));
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "loc": "x" }] },
            c,
        );
        assert_eq!(
            write_error(&reply),
            (
                16755,
                "Can't extract geo keys: { _id: 1, loc: \"x\" }  geo element must be an array or \
                 object: loc: \"x\""
                    .to_string()
            )
        );
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "loc": [200, 0] }] },
            c,
        );
        assert_eq!(
            write_error(&reply).1,
            "Can't extract geo keys: { _id: 1, loc: [ 200, 0 ] }  can't project geometry into \
             spherical CRS: [ 200, 0 ]"
        );
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{ "q": { "_id": 0 }, "u": { "$set": { "loc": 5 } } }] },
            c,
        );
        assert!(
            write_error(&reply).1.starts_with(
                "Plan executor error during update :: caused by :: Can't extract geo keys"
            ),
            "{reply}"
        );
        // Nothing to index is nothing to refuse.
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 2, "loc": Bson::Null }, { "_id": 3, "loc": [] }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(2), "{reply}");

        seed(c, vec![doc! { "_id": 0 }], Some("2d"));
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "loc": point(1.0, 1.0) }] },
            c,
        );
        assert_eq!(
            write_error(&reply),
            (
                16804,
                "location object expected, location array not in correct format".to_string()
            )
        );
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "loc": [[1, 1], [2, 2]] }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");

        // A build over a document the index cannot hold fails, and says
        // where its scan stopped.
        seed(
            c,
            vec![doc! { "_id": 0 }, doc! { "_id": 1, "loc": [200, 0] }],
            None,
        );
        let reply = dispatch_full(
            &doc! { "createIndexes": "c", "indexes": [{ "key": { "loc": "2d" }, "name": "geo" }] },
            c,
        );
        let (code, message) = refusal(&reply);
        assert_eq!(code, 13027);
        assert!(
            message.contains("collection scan stopped. totalRecords: 1;"),
            "{message}"
        );
        assert!(
            message.ends_with("point not in interval of [ -180, 180 ] :: caused by :: { _id: 1, loc: [ 200, 0 ] }"),
            "{message}"
        );
    });
}

#[test]
fn several_locations_are_found_through_a_2d_index() {
    with_wt(|c| {
        seed(
            c,
            vec![
                doc! { "_id": 1, "loc": [[1, 1], [30, 30]] },
                doc! { "_id": 2, "loc": [3, 0, 99] },
                doc! { "_id": 3, "loc": [9, 9] },
            ],
            Some("2d"),
        );
        let mut got = ids(&find(
            c,
            doc! { "loc": { "$geoWithin": { "$box": [[0, 0], [3, 3]] } } },
        ));
        got.sort();
        assert_eq!(got, [1, 2]);
        let mut got = ids(&find(
            c,
            doc! { "loc": { "$geoWithin": { "$box": [[29, 29], [31, 31]] } } },
        ));
        got.sort();
        assert_eq!(got, [1]);
    });
}
