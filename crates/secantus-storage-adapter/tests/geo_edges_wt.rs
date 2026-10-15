//! The edges of the geo surface over real WiredTiger: what a `2d` index's
//! options are and how they are listed, what an index build refuses, a
//! `$near` centre outside a `2d` index's range, `$geoNear` run inside
//! `$lookup` and `$unionWith`, and the sphere tested as mongod's S2 code
//! tests it. Every expectation is what mongod 8.2.11 answered to the same
//! command (2026-10-15).

mod common;

use bson::{doc, Bson, Decimal128, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;
use std::str::FromStr;

fn point(x: f64, y: f64) -> Document {
    doc! { "type": "Point", "coordinates": [x, y] }
}

fn line(a: [f64; 2], b: [f64; 2]) -> Document {
    doc! { "type": "LineString", "coordinates": [[a[0], a[1]], [b[0], b[1]]] }
}

fn ok(reply: &Document) {
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
}

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

fn insert(c: &mut CommandContext, coll: &str, docs: Vec<Document>) {
    let reply = dispatch_full(&doc! { "insert": coll, "documents": docs }, c);
    ok(&reply);
    assert!(!reply.contains_key("writeErrors"), "{reply}");
}

fn create(c: &mut CommandContext, coll: &str, indexes: Vec<Document>) -> Document {
    dispatch_full(&doc! { "createIndexes": coll, "indexes": indexes }, c)
}

fn names(c: &mut CommandContext, coll: &str) -> Vec<String> {
    let reply = dispatch_full(&doc! { "listIndexes": coll }, c);
    ok(&reply);
    let mut out: Vec<String> = specs(&reply)
        .iter()
        .map(|s| s.get_str("name").unwrap().to_string())
        .collect();
    out.sort();
    out
}

fn specs(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn listed(c: &mut CommandContext, coll: &str, name: &str) -> Document {
    let reply = dispatch_full(&doc! { "listIndexes": coll }, c);
    ok(&reply);
    specs(&reply)
        .into_iter()
        .find(|s| s.get_str("name") == Ok(name))
        .unwrap()
}

fn exists(c: &mut CommandContext, coll: &str) -> bool {
    let reply = dispatch_full(
        &doc! { "listCollections": 1, "filter": { "name": coll } },
        c,
    );
    ok(&reply);
    !specs(&reply).is_empty()
}

fn ids(c: &mut CommandContext, coll: &str, filter: Document) -> Vec<i32> {
    let reply = dispatch_full(&doc! { "find": coll, "filter": filter }, c);
    ok(&reply);
    let mut out: Vec<i32> = specs(&reply)
        .iter()
        .map(|d| d.get_i32("_id").unwrap())
        .collect();
    out.sort();
    out
}

#[test]
fn a_2d_index_refuses_options_it_cannot_hash_with() {
    with_wt(|c| {
        let refused = |c: &mut CommandContext, spec: Document| {
            let reply = create(c, "n", vec![spec]);
            let got = refusal(&reply);
            // Nothing is left behind, the collection included.
            assert!(!exists(c, "n"), "{reply}");
            got
        };
        let two_d = |extra: Document| {
            let mut spec = doc! { "key": { "loc": "2d" }, "name": "l" };
            spec.extend(extra);
            spec
        };
        assert_eq!(
            refused(c, two_d(doc! { "bits": 40 })),
            (
                72,
                "bits for hash must be > 0 and <= 32, but 40 bits were specified".into()
            )
        );
        // A fraction is cut off: 0.5 is 0 bits, and 2^40 is as many as fit.
        assert_eq!(
            refused(c, two_d(doc! { "bits": 0.5 })).1,
            "bits for hash must be > 0 and <= 32, but 0 bits were specified"
        );
        assert_eq!(
            refused(c, two_d(doc! { "bits": 1_i64 << 40 })).1,
            "bits for hash must be > 0 and <= 32, but 2147483647 bits were specified"
        );
        assert_eq!(
            refused(c, two_d(doc! { "min": 5, "max": 1 })),
            (
                72,
                "region for hash must be valid and have positive area, but [5, 1] was specified"
                    .into()
            )
        );
        assert_eq!(
            refused(c, two_d(doc! { "max": -200 })).1,
            "region for hash must be valid and have positive area, but [-180, -200] was specified"
        );
        assert_eq!(
            refused(c, two_d(doc! { "min": 0, "max": 1e-300 })),
            (72, "range [0, 1e-300] is too small.".into())
        );
        assert_eq!(
            refused(c, two_d(doc! { "min": -1e300, "max": 1e300 })),
            (4799400, "Invalid GeoHashConverter parameters".into())
        );
        // The type is checked whatever the index.
        assert_eq!(
            refused(c, doc! { "key": { "q": 1 }, "name": "q", "bits": "x" }),
            (
                14,
                "Error in specification { key: { q: 1 }, name: \"q\", bits: \"x\" } :: caused by \
                 :: The field 'bits' must be a number, but got string"
                    .into()
            )
        );
        assert_eq!(
            refused(c, doc! { "key": { "k": 1, "loc": "2d" }, "name": "i" }),
            (16801, "2d has to be first in index".into())
        );
        assert_eq!(
            refused(c, doc! { "key": { "loc": "2d", "at": "2d" }, "name": "i" }),
            (16800, "can't have 2 geo fields".into())
        );
        assert_eq!(
            refused(
                c,
                doc! { "key": { "loc": "2d", "k": "2dsphere" }, "name": "i" }
            )
            .1,
            "Error in specification { key: { loc: \"2d\", k: \"2dsphere\" }, name: \"i\" } :: \
             caused by :: Can't use more than one index plugin for a single index."
        );
        ok(&create(
            c,
            "ok",
            vec![two_d(doc! { "bits": 32, "min": 0, "max": 10 })],
        ));
    });
}

#[test]
fn an_index_build_on_an_existing_collection_says_so() {
    with_wt(|c| {
        insert(c, "b", vec![doc! { "_id": 1 }]);
        // The index's code refuses it when the build starts, and the build
        // names the first spec of the request.
        let reply = create(
            c,
            "b",
            vec![
                doc! { "key": { "z": 1 }, "name": "z" },
                doc! { "key": { "loc": "2d" }, "name": "l", "bits": 40 },
            ],
        );
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 72);
        assert!(errmsg.starts_with("Index build failed: "), "{errmsg}");
        assert!(
            errmsg.contains(":: caused by :: Caught exception during index builder ("),
            "{errmsg}"
        );
        assert!(
            errmsg.ends_with(
                "2 index specs provided. First index spec: { v: 2, key: { z: 1 }, name: \"z\" } \
                 :: caused by :: bits for hash must be > 0 and <= 32, but 40 bits were specified"
            ),
            "{errmsg}"
        );
        // Neither index was built.
        assert_eq!(names(c, "b"), ["_id_"]);
        // An empty collection's build says that.
        ok(&dispatch_full(&doc! { "create": "e" }, c));
        let reply = create(
            c,
            "e",
            vec![doc! { "key": { "loc": "2d" }, "name": "l", "bits": 40 }],
        );
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 72);
        assert!(
            errmsg.contains(":: caused by :: index build on empty collection failed: "),
            "{errmsg}"
        );
        assert!(
            errmsg.ends_with(
                ":: caused by :: bits for hash must be > 0 and <= 32, but 40 bits were specified"
            ),
            "{errmsg}"
        );
    });
}

#[test]
fn one_request_asking_for_one_index_twice() {
    with_wt(|c| {
        let twice = vec![
            doc! { "key": { "loc": "2dsphere" }, "name": "one" },
            doc! { "key": { "loc": "2dsphere" }, "name": "two" },
        ];
        assert_eq!(
            refusal(&create(c, "n", twice.clone())),
            (85, "Index already exists with a different name: one".into())
        );
        assert!(!exists(c, "n"));
        let same = vec![
            doc! { "key": { "a": 1 }, "name": "x" },
            doc! { "key": { "a": 1 }, "name": "x" },
        ];
        assert_eq!(
            refusal(&create(c, "n", same.clone())),
            (68, "Identical index already exists: x".into())
        );
        insert(c, "b", vec![doc! { "_id": 1 }]);
        let (code, errmsg) = refusal(&create(c, "b", same));
        assert_eq!(code, 96);
        assert!(
            errmsg.ends_with(
                ":: caused by :: Cannot build two identical indexes. Try again without duplicate \
                 indexes."
            ),
            "{errmsg}"
        );
        // The spec before the two is not built either.
        let mut three = vec![doc! { "key": { "z": 1 }, "name": "z" }];
        three.extend(twice);
        let (code, errmsg) = refusal(&create(c, "b", three));
        assert_eq!(code, 85);
        assert!(errmsg.starts_with("Index build failed: "), "{errmsg}");
        assert_eq!(names(c, "b"), ["_id_"]);
    });
}

#[test]
fn a_2dsphere_index_version_is_a_whole_number() {
    with_wt(|c| {
        let version =
            |v: Bson| doc! { "key": { "loc": "2dsphere" }, "name": "s", "2dsphereIndexVersion": v };
        assert_eq!(
            refusal(&create(c, "n", vec![version(Bson::Double(2.5))])),
            (
                2,
                "Expected field \"2dsphereIndexVersion\" to have a value exactly representable as \
                 a 64-bit integer, but found 2dsphereIndexVersion: 2.5"
                    .into()
            )
        );
        assert_eq!(
            refusal(&create(c, "n", vec![version(Bson::Double(f64::NAN))])).1,
            "Error in specification { key: { loc: \"2dsphere\" }, name: \"s\", \
             2dsphereIndexVersion: nan, v: 2 } :: caused by :: Invalid value for geo index \
             version { 2dsphereIndexVersion : 2dsphereIndexVersion: nan }, only versions: \
             [1,2,3] are supported"
        );
        assert_eq!(
            refusal(&create(c, "n", vec![version(Bson::Double(0.5))])).1,
            "Error in specification { key: { loc: \"2dsphere\" }, name: \"s\", \
             2dsphereIndexVersion: 0.5, v: 2 } :: caused by :: unsupported geo index version \
             { 2dsphereIndexVersion : 2dsphereIndexVersion: 0.5 }, only versions: [1,2,3] are \
             supported"
        );
        assert!(!exists(c, "n"));
        let three = Bson::Decimal128(Decimal128::from_str("3").unwrap());
        ok(&create(c, "n", vec![version(three)]));
        assert_eq!(
            listed(c, "n", "s").get("2dsphereIndexVersion"),
            Some(&Bson::Int32(3))
        );
    });
}

#[test]
fn list_indexes_gives_a_spec_in_mongods_order_and_types() {
    with_wt(|c| {
        ok(&create(
            c,
            "c",
            vec![
                doc! { "key": { "loc": "2d" }, "name": "l", "max": 10_i64, "sparse": true,
                "bits": 20_i64, "min": -10_i64 },
                doc! { "key": { "p": "2d" }, "name": "p", "bits": 2.5 },
                doc! { "key": { "g": "2dsphere" }, "name": "g", "collation": { "locale": "fr" },
                "bits": 26, "min": -10 },
                doc! { "key": { "q": "2d" }, "name": "q", "2dsphereIndexVersion": 3 },
            ],
        ));
        let l = listed(c, "c", "l");
        let keys: Vec<&str> = l.keys().map(String::as_str).collect();
        assert_eq!(keys, ["v", "key", "name", "sparse", "bits", "min", "max"]);
        assert_eq!(l.get("bits"), Some(&Bson::Int32(20)));
        assert_eq!(l.get("min"), Some(&Bson::Double(-10.0)));
        assert_eq!(l.get("max"), Some(&Bson::Double(10.0)));
        assert_eq!(listed(c, "c", "p").get("bits"), Some(&Bson::Int32(2)));
        // The options of another kind of index are not listed, and the
        // collation comes after the index version.
        let g = listed(c, "c", "g");
        let keys: Vec<&str> = g.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["v", "key", "name", "2dsphereIndexVersion", "collation"]
        );
        assert!(!listed(c, "c", "q").contains_key("2dsphereIndexVersion"));
    });
}

#[test]
fn a_near_centre_outside_a_2d_range() {
    with_wt(|c| {
        insert(
            c,
            "c",
            vec![
                doc! { "_id": 0, "loc": [1, 1] },
                doc! { "_id": 1, "loc": [5, 5] },
            ],
        );
        ok(&create(
            c,
            "c",
            vec![doc! { "key": { "loc": "2d" }, "name": "l", "min": -10, "max": 10 }],
        ));
        let why = "point not in interval of [ -10, 10 ] :: caused by :: { 0: 20.0, 1: 0.0 }";
        let near = doc! { "loc": { "$near": [20, 0] } };
        assert_eq!(
            refusal(&dispatch_full(
                &doc! { "find": "c", "filter": near.clone() },
                c
            )),
            (
                16433,
                format!("Executor error during find command: t.c :: caused by :: {why}")
            )
        );
        // The two ends of the range are in it.
        assert_eq!(ids(c, "c", doc! { "loc": { "$near": [10, 0] } }), [0, 1]);
        assert_eq!(
            refusal(&dispatch_full(
                &doc! { "count": "c", "query": near.clone() },
                c
            ))
            .1,
            format!("Executor error during count command on namespace: t.c :: caused by :: {why}")
        );
        assert_eq!(
            refusal(&dispatch_full(
                &doc! { "findAndModify": "c", "query": near.clone(), "update": { "$set": { "z": 1 } } },
                c
            ))
            .1,
            format!("Plan executor error during findAndModify :: caused by :: {why}")
        );
        // A write that cannot run its `$near` changes nothing.
        let reply = dispatch_full(
            &doc! { "delete": "c", "deletes": [{ "q": near.clone(), "limit": 0 }] },
            c,
        );
        ok(&reply);
        assert_eq!(reply.get_i32("n"), Ok(0));
        let error = reply.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(error.get_i32("code"), Ok(16433));
        assert_eq!(
            error.get_str("errmsg").unwrap(),
            format!("Plan executor error during delete :: caused by :: {why}")
        );
        assert_eq!(ids(c, "c", doc! {}), [0, 1]);
        let reply = dispatch_full(
            &doc! { "aggregate": "c", "pipeline": [{ "$geoNear": { "near": [20, 0], "distanceField": "d" } }], "cursor": {} },
            c,
        );
        assert_eq!(
            refusal(&reply).1,
            format!(
                "Executor error during aggregate command on namespace: t.c :: caused by :: {why}"
            )
        );
        // Beside a 2dsphere index, mongod meets it trying the two plans.
        insert(c, "b", vec![doc! { "_id": 0, "loc": [1, 1] }]);
        ok(&create(
            c,
            "b",
            vec![doc! { "key": { "loc": "2d" }, "name": "l", "min": -10, "max": 10.5 }],
        ));
        ok(&create(
            c,
            "b",
            vec![doc! { "key": { "loc": "2dsphere" }, "name": "s" }],
        ));
        let reply = dispatch_full(
            &doc! { "find": "b", "filter": { "loc": { "$nearSphere": [20, 0] } } },
            c,
        );
        assert_eq!(
            refusal(&reply).1,
            "error while multiplanner was selecting best plan :: caused by :: point not in \
             interval of [ -10, 10.5 ] :: caused by :: { 0: 20.0, 1: 0.0 }"
        );
        // A write with no index its `$near` can read is refused too.
        insert(c, "n", vec![doc! { "_id": 0, "loc": [1, 1] }]);
        let reply = dispatch_full(
            &doc! { "delete": "n", "deletes": [{ "q": { "loc": { "$near": [0, 0] } }, "limit": 1 }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(0), "{reply}");
        assert_eq!(ids(c, "n", doc! {}), [0]);
        // A collection that does not exist holds nothing to plan for.
        assert_eq!(
            ids(c, "nope", doc! { "loc": { "$near": [0, 0] } }),
            Vec::<i32>::new()
        );
    });
}

#[test]
fn geo_near_inside_lookup_and_union_with() {
    with_wt(|c| {
        insert(c, "o", vec![doc! { "_id": 1 }]);
        insert(c, "g", vec![doc! { "_id": 1, "loc": point(0.0, 0.0) }]);
        insert(c, "h", vec![doc! { "_id": 1, "loc": [0, 0] }]);
        ok(&create(
            c,
            "h",
            vec![doc! { "key": { "loc": "2d" }, "name": "l", "min": -10, "max": 10 }],
        ));
        let gn = doc! { "$geoNear": { "near": point(0.0, 0.0), "distanceField": "d", "spherical": true } };
        let run = |c: &mut CommandContext, stage: Document| {
            refusal(&dispatch_full(
                &doc! { "aggregate": "o", "pipeline": [stage], "cursor": {} },
                c,
            ))
        };
        let wrapped = |why: &str| {
            format!(
                "Executor error during aggregate command on namespace: t.o :: caused by :: {why}"
            )
        };
        let none = "$geoNear requires a 2d or 2dsphere index, but none were found";
        assert_eq!(
            run(
                c,
                doc! { "$lookup": { "from": "g", "pipeline": [gn.clone()], "as": "x" } }
            ),
            (27, wrapped(none))
        );
        assert_eq!(
            run(
                c,
                doc! { "$unionWith": { "coll": "g", "pipeline": [gn.clone()] } }
            ),
            (27, wrapped(none))
        );
        assert_eq!(
            run(
                c,
                doc! { "$unionWith": { "coll": "nope", "pipeline": [gn.clone()] } }
            ),
            (
                26,
                wrapped("$geoNear requires a geo index to run, but t.nope does not exist")
            )
        );
        let far = doc! { "$geoNear": { "near": [20, 0], "distanceField": "d" } };
        let why = "point not in interval of [ -10, 10 ] :: caused by :: { 0: 20.0, 1: 0.0 }";
        assert_eq!(
            run(
                c,
                doc! { "$lookup": { "from": "h", "pipeline": [far.clone()], "as": "x" } }
            )
            .1,
            wrapped(&format!(
                "error while multiplanner was selecting best plan :: caused by :: {why}"
            ))
        );
        assert_eq!(
            run(c, doc! { "$unionWith": { "coll": "h", "pipeline": [far] } }).1,
            wrapped(why)
        );
        // Run for nothing, the stage has nothing to refuse.
        let reply = dispatch_full(
            &doc! { "aggregate": "nothing", "pipeline": [{ "$lookup": { "from": "g", "pipeline": [gn], "as": "x" } }], "cursor": {} },
            c,
        );
        ok(&reply);
    });
}

#[test]
fn the_sphere_as_s2_tests_it() {
    with_wt(|c| {
        insert(
            c,
            "c",
            vec![
                doc! { "_id": 1, "loc": line([-80.0, 20.0], [80.0, 20.0]) },
                doc! { "_id": 2, "loc": point(0.0, 20.0) },
                doc! { "_id": 3, "loc": line([170.0, 0.0], [-170.0, 0.0]) },
                doc! { "_id": 4, "loc": line([170.0, 5.0], [-170.0, 5.0]) },
            ],
        );
        // Both ends of the line are in the cap and its arc bows out of it.
        let cap = |c: &mut CommandContext, centre: [f64; 2], r: f64| {
            ids(
                c,
                "c",
                doc! { "loc": { "$geoWithin": { "$centerSphere": [[centre[0], centre[1]], r] } } },
            )
        };
        assert_eq!(cap(c, [0.0, -90.0], 2.0), [2, 4]);
        // mongod's projection of the pole onto the equator is rounding, and
        // puts the line's nearest point near the other pole.
        assert_eq!(cap(c, [180.0, -90.0], 2.0), [2, 3, 4]);

        // A point is tested as its leaf cell: one a centimetre off a corner,
        // outside, is not in; one closer than a leaf cell to the cap is.
        insert(
            c,
            "p",
            vec![
                doc! { "_id": 1, "loc": point(-10.0, -10.0000001) },
                doc! { "_id": 2, "loc": point(-10.0, -10.0) },
                doc! { "_id": 3, "loc": point(-10.0 - 1e-7, -10.0) },
            ],
        );
        let square = doc! { "type": "Polygon", "coordinates": [[[-10, -10], [20, -10], [20, 20], [-10, 20], [-10, -10]]] };
        assert_eq!(
            ids(
                c,
                "p",
                doc! { "loc": { "$geoWithin": { "$geometry": square.clone() } } }
            ),
            [2]
        );
        assert_eq!(
            ids(
                c,
                "p",
                doc! { "loc": { "$geoWithin": { "$centerSphere": [[-10, -10], 1e-9] } } }
            ),
            [1, 2, 3]
        );
    });
}

#[test]
fn the_strictwinding_crs_is_a_big_polygon() {
    with_wt(|c| {
        let docs = vec![
            doc! { "_id": 1, "loc": point(0.0, 0.0) },
            doc! { "_id": 2, "loc": point(100.0, 50.0) },
            doc! { "_id": 3, "loc": point(-10.0, 5.0) },
            doc! { "_id": 9, "loc": point(-10.0, -10.0000001) },
            doc! { "_id": 10, "loc": line([-20.0, 0.0], [30.0, 0.0]) },
        ];
        insert(c, "c", docs.clone());
        insert(c, "i", docs);
        ok(&create(
            c,
            "i",
            vec![doc! { "key": { "loc": "2dsphere" }, "name": "s" }],
        ));
        let strict = doc! { "type": "name", "properties": { "name": "urn:x-mongodb:crs:strictwinding:EPSG:4326" } };
        let clockwise = Bson::Array(vec![Bson::Array(vec![
            bson::bson!([-10, -10]),
            bson::bson!([-10, 20]),
            bson::bson!([20, 20]),
            bson::bson!([20, -10]),
            bson::bson!([-10, -10]),
        ])]);
        let big =
            doc! { "type": "Polygon", "coordinates": clockwise.clone(), "crs": strict.clone() };
        for coll in ["c", "i"] {
            assert_eq!(
                ids(
                    c,
                    coll,
                    doc! { "loc": { "$geoWithin": { "$geometry": big.clone() } } }
                ),
                [2, 3, 9]
            );
            assert_eq!(
                ids(
                    c,
                    coll,
                    doc! { "loc": { "$geoIntersects": { "$geometry": big.clone() } } }
                ),
                [2, 3, 9, 10]
            );
            // Without the crs a polygon is the smaller side of its loop.
            let small = doc! { "type": "Polygon", "coordinates": clockwise.clone() };
            assert_eq!(
                ids(
                    c,
                    coll,
                    doc! { "loc": { "$geoWithin": { "$geometry": small } } }
                ),
                [1, 3]
            );
        }
        let refused = |c: &mut CommandContext, geometry: Document| {
            refusal(&dispatch_full(
                &doc! { "find": "c", "filter": { "loc": { "$geoIntersects": { "$geometry": geometry } } } },
                c,
            ))
        };
        assert_eq!(
            refused(
                c,
                doc! { "type": "Point", "coordinates": [0, 0], "crs": strict.clone() }
            ),
            (
                2,
                "Strict winding order CRS is only supported by polygon".into()
            )
        );
        assert_eq!(
            refused(
                c,
                doc! { "type": "Point", "coordinates": [0, 0], "crs": { "type": "name", "properties": { "name": "foo" } } }
            ),
            (2, "Unknown CRS name: foo".into())
        );
        assert_eq!(
            refused(c, doc! { "type": "Point", "coordinates": [0, 0], "crs": 1 }),
            (
                2,
                "GeoJSON CRS must be an object, instead got type int".into()
            )
        );
        // A document's big polygon matches nothing, and no index holds one.
        insert(c, "s", vec![doc! { "_id": 1, "loc": big.clone() }]);
        assert_eq!(
            ids(
                c,
                "s",
                doc! { "loc": { "$geoIntersects": { "$geometry": point(0.0, 0.0) } } }
            ),
            Vec::<i32>::new()
        );
        let reply = create(
            c,
            "s",
            vec![doc! { "key": { "loc": "2dsphere" }, "name": "s" }],
        );
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 16755);
        assert!(
            errmsg.ends_with("can't index geometry with strict winding order"),
            "{errmsg}"
        );
    });
}

#[test]
fn an_index_finds_what_bows_past_the_vertices() {
    with_wt(|c| {
        // The edges of this polygon bow north of its vertices, and so does
        // the polygon: an index lookup by the box of its vertices found none.
        let docs: Vec<Document> = [60.0, 63.0, 65.0, 67.0, 69.0, 71.0, 73.0]
            .iter()
            .enumerate()
            .map(|(i, lat)| doc! { "_id": i as i32, "loc": point(0.0, *lat) })
            .collect();
        insert(c, "i", docs);
        ok(&create(
            c,
            "i",
            vec![doc! { "key": { "loc": "2dsphere" }, "name": "s" }],
        ));
        let bowed = doc! { "type": "Polygon", "coordinates": [[[-80, 20], [80, 20], [80, 30], [-80, 30], [-80, 20]]] };
        assert_eq!(
            ids(
                c,
                "i",
                doc! { "loc": { "$geoWithin": { "$geometry": bowed } } }
            ),
            [2, 3, 4, 5, 6]
        );
    });
}

#[test]
fn a_boundary_shared_is_not_an_intersection() {
    with_wt(|c| {
        let square = [[1, 1], [11, 1], [11, 11], [1, 11], [1, 1]];
        let polygon = |ring: &[[i32; 2]]| {
            doc! { "type": "Polygon", "coordinates": [ring.iter().map(|p| bson::bson!([p[0], p[1]])).collect::<Vec<Bson>>()] }
        };
        insert(
            c,
            "c",
            vec![
                doc! { "_id": 1, "loc": line([1.0, 1.0], [11.0, 1.0]) },
                doc! { "_id": 5, "loc": line([0.0, 5.0], [1.0, 5.0]) },
                doc! { "_id": 7, "loc": line([1.0, 1.0], [0.0, 0.0]) },
                doc! { "_id": 9, "loc": line([5.0, 5.0], [5.0, 1.0]) },
                doc! { "_id": 11, "loc": polygon(&square) },
                doc! { "_id": 12, "loc": polygon(&[[11, 1], [21, 1], [21, 11], [11, 11], [11, 1]]) },
                doc! { "_id": 13, "loc": polygon(&[[11, 11], [21, 11], [21, 21], [11, 21], [11, 11]]) },
                doc! { "_id": 15, "loc": polygon(&[[1, 1], [5, 1], [5, 5], [1, 5], [1, 1]]) },
            ],
        );
        let query = doc! { "loc": { "$geoIntersects": { "$geometry": polygon(&square) } } };
        assert_eq!(ids(c, "c", query), [1, 9, 11, 15]);
    });
}

/// mongod parses a `$geoNear` query before it looks for an index: with no
/// index the point can use, an unknown top-level operator is 2, and only a
/// query that parses is refused 291 (measured 8.2.11, 2026-10-15).
#[test]
fn geo_near_parses_its_query_before_it_plans() {
    with_wt(|c| {
        insert(c, "c", vec![doc! { "_id": 0, "loc": [1, 1] }]);
        ok(&create(
            c,
            "c",
            vec![doc! { "key": { "loc": "2d" }, "name": "l" }],
        ));
        let run = |c: &mut CommandContext, query: Document| {
            let stage = doc! { "near": point(0.0, 0.0), "distanceField": "d", "query": query };
            dispatch_full(
                &doc! { "aggregate": "c", "pipeline": [{ "$geoNear": stage }], "cursor": {} },
                c,
            )
        };
        let (code, errmsg) = refusal(&run(c, doc! { "$nope": 1 }));
        assert_eq!(code, 2);
        assert_eq!(
            errmsg,
            "unknown top level operator: $nope. If you have a field name that starts with a \
             '$' symbol, consider using $getField or $setField."
        );
        let (code, _) = refusal(&run(c, doc! { "$or": [{ "$nope": 1 }] }));
        assert_eq!(code, 2);
        let (code, _) = refusal(&run(c, doc! { "k": 1 }));
        assert_eq!(code, 291);
    });
}
