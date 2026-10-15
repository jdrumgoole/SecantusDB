//! The order a read through a geo index returns documents in, over real
//! WiredTiger. Every expectation is mongod 8.2.11's answer to the same
//! command over the same documents (`tools/probes/geo_queries.py`, and the
//! commands below run against it).
//!
//! mongod reads a `$geoWithin` or `$geoIntersects` from a geo index in the
//! order of the index's keys (S2 cells, signed, for `2dsphere`; geohashes
//! for `2d`), a document where its first key in the scan falls; a hint to
//! a geo index reads all of it, and returns only what it holds; a `$near`
//! is a search in widening rings whose queue decides the order of documents
//! at one distance.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn point(x: f64, y: f64) -> Document {
    doc! { "type": "Point", "coordinates": [x, y] }
}

fn line(a: (f64, f64), b: (f64, f64)) -> Document {
    doc! { "type": "LineString", "coordinates": [[a.0, a.1], [b.0, b.1]] }
}

fn square() -> Document {
    doc! { "type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]]] }
}

/// The geo probe's `SPHERE` documents.
fn sphere() -> Vec<Document> {
    vec![
        doc! { "_id": 4, "loc": point(1.0, 1.0), "k": "a" },
        doc! { "_id": 1, "loc": point(0.0, 0.0), "k": "b" },
        doc! { "_id": 9, "loc": point(10.0, 10.0), "k": "a" },
        doc! { "_id": 2, "loc": point(2.5, 2.5), "k": "b" },
        doc! { "_id": 7, "loc": point(-73.97, 40.77), "k": "a" },
        doc! { "_id": 3, "loc": point(-73.88, 40.78), "k": "b" },
        doc! { "_id": 12, "loc": point(179.9, 0.0), "k": "a" },
        doc! { "_id": 5, "loc": point(-179.9, 0.0), "k": "b" },
        doc! { "_id": 8, "loc": point(0.0, 89.9), "k": "a" },
        doc! { "_id": 6, "loc": line((0.0, 0.0), (2.0, 2.0)), "k": "b" },
        doc! { "_id": 11, "loc": square(), "k": "a" },
        doc! { "_id": 10, "loc": { "type": "Polygon", "coordinates": [[[5, 5], [6, 5], [6, 6], [5, 6], [5, 5]]] }, "k": "b" },
        doc! { "_id": 13, "loc": [1.5, 1.5], "k": "a" },
        doc! { "_id": 14, "loc": { "lng": 2, "lat": 2 }, "k": "b" },
        doc! { "_id": 15, "k": "a" },
        doc! { "_id": 16, "loc": Bson::Null, "k": "b" },
        doc! { "_id": 17, "loc": { "type": "MultiPoint", "coordinates": [[1, 1], [30, 30]] }, "k": "a" },
        doc! { "_id": 18, "loc": point(3.0, 3.0), "k": "b" },
        doc! { "_id": 19, "loc": point(0.0, 3.0), "k": "a" },
        doc! { "_id": 20, "loc": point(1.0, 1.0), "k": "b" },
    ]
}

/// The geo probe's `FLAT` documents (legacy pairs, for a `2d` index).
fn flat() -> Vec<Document> {
    vec![
        doc! { "_id": 4, "loc": [1, 1], "k": "a" },
        doc! { "_id": 1, "loc": [0, 0], "k": "b" },
        doc! { "_id": 9, "loc": [10, 10], "k": "a" },
        doc! { "_id": 2, "loc": [2.5, 2.5], "k": "b" },
        doc! { "_id": 7, "loc": { "x": -73.97, "y": 40.77 }, "k": "a" },
        doc! { "_id": 3, "loc": { "lng": -73.88, "lat": 40.78 }, "k": "b" },
        doc! { "_id": 12, "loc": [179.9, 0], "k": "a" },
        doc! { "_id": 5, "loc": [-179.9, 0], "k": "b" },
        doc! { "_id": 8, "loc": [0, 89.9], "k": "a" },
        doc! { "_id": 13, "loc": [1.5, 1.5], "k": "a" },
        doc! { "_id": 15, "k": "a" },
        doc! { "_id": 16, "loc": Bson::Null, "k": "b" },
        doc! { "_id": 17, "loc": [[1, 1], [30, 30]], "k": "a" },
        doc! { "_id": 18, "loc": [3, 3], "k": "b" },
        doc! { "_id": 19, "loc": [0, 3], "k": "a" },
        doc! { "_id": 20, "loc": [1, 1], "k": "b" },
        doc! { "_id": 21, "loc": [3, 0, 99], "k": "a" },
    ]
}

fn seed(c: &mut CommandContext, docs: Vec<Document>, indexes: Vec<Document>) {
    dispatch_full(&doc! { "drop": "c" }, c);
    let reply = dispatch_full(&doc! { "insert": "c", "documents": docs }, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    assert!(!reply.contains_key("writeErrors"), "{reply}");
    if !indexes.is_empty() {
        let reply = dispatch_full(&doc! { "createIndexes": "c", "indexes": indexes }, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    }
}

fn geo(kind: &str) -> Document {
    doc! { "key": { "loc": kind }, "name": "geo" }
}

fn ids(reply: &Document) -> Vec<i32> {
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

fn find(c: &mut CommandContext, filter: Document) -> Vec<i32> {
    ids(&dispatch_full(&doc! { "find": "c", "filter": filter }, c))
}

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

#[test]
fn a_2dsphere_scan_returns_documents_in_cell_order() {
    with_wt(|c| {
        seed(c, sphere(), vec![geo("2dsphere")]);
        let within = |shape: Document| doc! { "loc": { "$geoWithin": shape } };
        // Points by their leaf cells; the line and the polygon by the
        // coarser cells of their coverings, which come first here.
        assert_eq!(
            find(c, within(doc! { "$centerSphere": [[0, 0], 0.05] })),
            [6, 1, 4, 20, 13, 14]
        );
        assert_eq!(
            find(c, within(doc! { "$geometry": square() })),
            [11, 1, 6, 4, 20, 13, 14, 2, 18, 19]
        );
        // Faces 4 and 5 hold the cells with the top bit set: as signed keys
        // they come before the rest (New York, 7 and 3, first).
        assert_eq!(
            find(c, within(doc! { "$centerSphere": [[0, 0], 4] }))[..6],
            [7, 3, 6, 11, 1, 4]
        );
        let intersects = |shape: Bson| doc! { "loc": { "$geoIntersects": { "$geometry": shape } } };
        assert_eq!(find(c, intersects(point(1.0, 1.0).into())), [4, 20, 17, 11]);
        // A legacy pair as the geometry is the same point.
        assert_eq!(find(c, intersects(bson::bson!([1, 1]))), [4, 20, 17, 11]);
        assert_eq!(
            find(c, intersects(line((0.0, 3.0), (3.0, 0.0)).into())),
            [11, 6, 19]
        );
        // The rest of the query filters the scan; it does not reorder it.
        assert_eq!(
            find(
                c,
                doc! { "loc": { "$geoWithin": { "$geometry": square() } }, "k": "b" }
            ),
            [1, 6, 20, 14, 2, 18]
        );
    });
}

#[test]
fn a_big_polygon_is_covered_as_given() {
    with_wt(|c| {
        seed(c, sphere(), vec![geo("2dsphere")]);
        let strict = doc! {
            "type": "Polygon",
            "coordinates": [[[-10, -10], [20, -10], [20, 20], [-10, 20], [-10, -10]]],
            "crs": { "type": "name", "properties": {
                "name": "urn:x-mongodb:crs:strictwinding:EPSG:4326" } },
        };
        assert_eq!(
            find(c, doc! { "loc": { "$geoWithin": { "$geometry": strict } } }),
            [6, 11, 1, 4, 20, 13, 14, 2, 18, 10, 19, 9]
        );
    });
}

#[test]
fn a_2d_scan_returns_documents_in_geohash_order() {
    with_wt(|c| {
        seed(c, flat(), vec![geo("2d")]);
        let within = |shape: Document| doc! { "loc": { "$geoWithin": shape } };
        // Document 17 holds two points: it comes where (1, 1) does.
        assert_eq!(
            find(c, within(doc! { "$box": [[0, 0], [3, 3]] })),
            [1, 4, 17, 20, 13, 2, 19, 21, 18]
        );
        assert_eq!(
            find(c, within(doc! { "$box": [[-180, -90], [180, 90]] })),
            [5, 7, 3, 1, 4, 17, 20, 13, 2, 19, 21, 18, 9, 8, 12]
        );
        assert_eq!(
            find(c, within(doc! { "$centerSphere": [[180, 0], 0.01] })),
            [5, 12]
        );
    });
}

#[test]
fn a_hint_to_a_geo_index_reads_what_it_holds() {
    with_wt(|c| {
        seed(
            c,
            sphere(),
            vec![geo("2dsphere"), doc! { "key": { "k": 1 }, "name": "k_1" }],
        );
        let hinted = |filter: Document| doc! { "find": "c", "filter": filter, "hint": "geo" };
        // Documents 15 (no `loc`) and 16 (`loc: null`) are not in the index.
        assert_eq!(
            ids(&dispatch_full(&hinted(doc! {}), c)),
            [7, 3, 6, 11, 1, 4, 20, 17, 13, 14, 2, 18, 10, 19, 9, 8, 12, 5]
        );
        assert_eq!(
            ids(&dispatch_full(&hinted(doc! { "k": "a" }), c)),
            [7, 11, 4, 17, 13, 19, 9, 8, 12]
        );
        // A `$natural` hint reads the collection: insertion order.
        let natural = dispatch_full(
            &doc! { "find": "c", "hint": { "$natural": 1 },
            "filter": { "loc": { "$geoWithin": { "$geometry": square() } } } },
            c,
        );
        assert_eq!(ids(&natural), [4, 1, 2, 6, 11, 13, 14, 18, 19, 20]);
    });
}

#[test]
fn a_near_hinted_elsewhere_is_refused() {
    with_wt(|c| {
        seed(
            c,
            sphere(),
            vec![geo("2dsphere"), doc! { "key": { "k": 1 }, "name": "k_1" }],
        );
        let near = doc! { "loc": { "$near": { "$geometry": point(0.0, 0.0) } } };
        let hinted = |hint: Bson| doc! { "find": "c", "filter": near.clone(), "hint": hint };
        let (code, message) = refusal(&dispatch_full(&hinted("k_1".into()), c));
        assert_eq!(code, 291);
        assert_eq!(
            message,
            "error processing query: ns=t.cTree: GEONEAR  field=loc maxdist=1.79769e+308 \
             isNearSphere=0\nSort: {}\nProj: {}\n planner returned error :: caused by :: \
             unable to find index for $geoNear query"
        );
        let (code, message) = refusal(&dispatch_full(&hinted(doc! { "$natural": 1 }.into()), c));
        assert_eq!(
            (code, message.as_str()),
            (2, "geoNear expression not allowed with $natural hint")
        );
        let (code, message) = refusal(&dispatch_full(&hinted("nope".into()), c));
        assert_eq!(code, 2);
        assert!(
            message.ends_with(
                "planner returned error :: caused by :: hint provided does not correspond to \
                 an existing index"
            ),
            "{message}"
        );
        // Hinted to its own index, it is the search's order.
        assert_eq!(
            ids(&dispatch_full(&hinted("geo".into()), c)),
            [1, 11, 6, 20, 4, 17, 13, 14, 19, 2, 18, 10, 9, 3, 7, 8, 12, 5]
        );
        // The same for `$geoNear`, whose hint is the aggregate's.
        let stage = |hint: Bson| {
            doc! { "aggregate": "c", "hint": hint, "cursor": {}, "pipeline": [
                { "$geoNear": { "near": point(0.0, 0.0), "distanceField": "d" } },
            ] }
        };
        let (code, message) = refusal(&dispatch_full(&stage("k_1".into()), c));
        assert_eq!(code, 291);
        assert!(
            message.contains("Collation: { locale: \"simple\" }"),
            "{message}"
        );
        assert_eq!(
            refusal(&dispatch_full(&stage(doc! { "$natural": 1 }.into()), c)).0,
            2
        );
        assert_eq!(
            ids(&dispatch_full(&stage("geo".into()), c)),
            [1, 11, 6, 20, 4, 17, 13, 14, 19, 2, 18, 10, 9, 3, 7, 8, 12, 5]
        );
    });
}

#[test]
fn near_returns_ties_in_its_searchs_order() {
    with_wt(|c| {
        seed(c, sphere(), vec![geo("2dsphere")]);
        let near = |extra: Document| {
            let mut spec = doc! { "$geometry": point(0.0, 0.0) };
            spec.extend(extra);
            doc! { "loc": { "$near": spec } }
        };
        // 20 and 4 are one point, and 17 holds it: mongod's queue hands back
        // 20 first.
        assert_eq!(
            find(c, near(doc! {})),
            [1, 11, 6, 20, 4, 17, 13, 14, 19, 2, 18, 10, 9, 3, 7, 8, 12, 5]
        );
        // The rest of the query filters what the search returns.
        let mut filter = near(doc! {});
        filter.insert("k", "a");
        assert_eq!(find(c, filter), [11, 4, 17, 13, 19, 9, 7, 8, 12]);
        let reply = dispatch_full(
            &doc! { "aggregate": "c", "cursor": {}, "pipeline": [{ "$geoNear": {
            "near": point(0.0, 0.0), "distanceField": "d", "query": { "k": "a" } } }] },
            c,
        );
        assert_eq!(ids(&reply), [11, 4, 17, 13, 19, 9, 7, 8, 12]);

        // Four points at one distance from the centre of a `2d` search.
        seed(
            c,
            vec![
                doc! { "_id": 0, "loc": [1, 0] },
                doc! { "_id": 1, "loc": [0, 1] },
                doc! { "_id": 2, "loc": [-1, 0] },
                doc! { "_id": 3, "loc": [0, -1] },
            ],
            vec![geo("2d")],
        );
        assert_eq!(find(c, doc! { "loc": { "$near": [0, 0] } }), [2, 3, 0, 1]);

        seed(c, flat(), vec![geo("2d")]);
        assert_eq!(
            find(c, doc! { "loc": { "$near": [0, 0] } }),
            [1, 4, 17, 20, 13, 21, 19, 2, 18, 9, 3, 7, 8, 5, 12]
        );
        // A sort over the search keeps its ties in the search's order.
        let sorted = dispatch_full(
            &doc! { "find": "c", "filter": { "loc": { "$near": [0, 0] } }, "sort": { "k": 1 } },
            c,
        );
        assert_eq!(
            ids(&sorted),
            [4, 17, 13, 21, 19, 9, 7, 8, 12, 1, 20, 2, 18, 3, 5]
        );
    });
}

#[test]
fn a_bounded_geo_near_keeps_a_shape_whose_vertices_are_outside() {
    with_wt(|c| {
        // The line passes 0.5 degrees (about 55 km) from the centre and the
        // polygon holds it; every vertex of both is hundreds of km away.
        seed(
            c,
            vec![
                doc! { "_id": 1, "loc": point(0.0, 0.0) },
                doc! { "_id": 2, "loc": line((-5.0, 0.5), (5.0, 0.5)) },
                doc! { "_id": 3, "loc": { "type": "Polygon", "coordinates":
                [[[-5, -5], [5, -5], [5, 5], [-5, 5], [-5, -5]]] } },
                doc! { "_id": 4, "loc": point(3.0, 0.0) },
                doc! { "_id": 5, "loc": line((-5.0, 3.0), (5.0, 3.0)) },
            ],
            vec![geo("2dsphere")],
        );
        let stage = |spec: Document| {
            doc! { "aggregate": "c", "pipeline": [{ "$geoNear": spec }], "cursor": {} }
        };
        let reply = dispatch_full(
            &stage(doc! { "near": point(0.0, 0.0), "distanceField": "d", "maxDistance": 100000 }),
            c,
        );
        assert_eq!(ids(&reply), [3, 1, 2]);
        // A legacy point on the sphere gives the distance in radians.
        let reply = dispatch_full(
            &stage(
                doc! { "near": [0, 0], "distanceField": "d", "spherical": true,
                "maxDistance": 0.02 },
            ),
            c,
        );
        assert_eq!(ids(&reply), [3, 1, 2]);
    });
}
