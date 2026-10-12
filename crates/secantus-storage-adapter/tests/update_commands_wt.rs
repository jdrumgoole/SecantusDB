//! `update` and `findAndModify` over real WiredTiger: what the update
//! operators store, which element the positional operator names, what the
//! commands accept and refuse. Every expectation is what mongod 8.2.11
//! answered to the same command (`tools/probes/update_commands.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn seed(c: &mut CommandContext, docs: Vec<Document>) {
    dispatch_full(&doc! { "drop": "c" }, c);
    if !docs.is_empty() {
        let reply = dispatch_full(&doc! { "insert": "c", "documents": docs }, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        assert!(!reply.contains_key("writeErrors"), "{reply}");
    }
}

fn stored(c: &mut CommandContext) -> Vec<Document> {
    let reply = dispatch_full(&doc! { "find": "c", "sort": { "_id": 1 } }, c);
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn one(c: &mut CommandContext) -> Document {
    let mut all = stored(c);
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

fn statement(c: &mut CommandContext, statement: Document) -> Document {
    dispatch_full(&doc! { "update": "c", "updates": [statement] }, c)
}

fn update(c: &mut CommandContext, q: Document, u: impl Into<Bson>) -> Document {
    statement(c, doc! { "q": q, "u": u.into() })
}

/// `(n, nModified)` of a reply with no write error.
fn applied(reply: &Document) -> (i32, i32) {
    assert!(!reply.contains_key("writeErrors"), "{reply}");
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    (
        reply.get_i32("n").unwrap(),
        reply.get_i32("nModified").unwrap(),
    )
}

/// `(code, errmsg)` of the one write error a reply holds.
fn write_error(reply: &Document) -> (i32, String) {
    let errors = reply
        .get_array("writeErrors")
        .unwrap_or_else(|_| panic!("no write error: {reply}"));
    assert_eq!(errors.len(), 1, "{reply}");
    let e = errors[0].as_document().unwrap();
    (
        e.get_i32("code").unwrap(),
        e.get_str("errmsg").unwrap().to_string(),
    )
}

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

const EXEC: &str = "Plan executor error during update :: caused by :: ";

fn keys(d: &Document) -> Vec<&str> {
    d.keys().map(String::as_str).collect()
}

/// A stored sub-document that looks like extended JSON is a sub-document. It
/// came back from the decoder as the value it would stand for, so an update of
/// ANY field of the document rewrote it: `{a: {$numberLong: "1"}}` became
/// `{a: 1}`.
#[test]
fn a_sub_document_shaped_like_extended_json_survives_an_update() {
    with_wt(|c| {
        let shapes = [
            doc! { "$numberLong": "1" },
            doc! { "$oid": "0123456789abcdef01234567" },
            doc! { "$date": 5 },
            doc! { "$numberInt": "3" },
        ];
        for shape in shapes {
            let original = doc! { "_id": 1, "a": shape.clone(), "l": [shape.clone()] };

            seed(c, vec![original.clone()]);
            assert_eq!(
                applied(&update(c, doc! {}, doc! { "$set": { "z": 1 } })),
                (1, 1)
            );
            let after = one(c);
            assert_eq!(
                after.get("a"),
                Some(&Bson::Document(shape.clone())),
                "{shape}"
            );
            assert_eq!(
                after.get_array("l").unwrap()[0],
                Bson::Document(shape.clone())
            );

            seed(c, vec![original.clone()]);
            update(c, doc! {}, vec![doc! { "$set": { "z": 1 } }]);
            assert_eq!(
                one(c).get("a"),
                Some(&Bson::Document(shape.clone())),
                "{shape}"
            );

            seed(c, vec![original.clone()]);
            let reply = dispatch_full(
                &doc! { "findAndModify": "c", "query": {}, "update": { "$inc": { "n": 1 } }, "new": true },
                c,
            );
            assert_eq!(
                reply.get_document("value").unwrap().get("a"),
                Some(&Bson::Document(shape.clone())),
                "{reply}"
            );

            // Written by the update itself.
            seed(c, vec![doc! { "_id": 1 }]);
            update(
                c,
                doc! {},
                doc! { "$set": { "a": shape.clone() }, "$push": { "l": shape.clone() } },
            );
            let after = one(c);
            assert_eq!(
                after.get("a"),
                Some(&Bson::Document(shape.clone())),
                "{shape}"
            );
            assert_eq!(
                after.get_array("l").unwrap()[0],
                Bson::Document(shape.clone())
            );

            // And read through a pipeline.
            seed(c, vec![original]);
            let reply = dispatch_full(
                &doc! { "aggregate": "c", "pipeline": [{ "$addFields": { "q": 1 } }], "cursor": {} },
                c,
            );
            let batch = reply
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap();
            assert_eq!(
                batch[0].as_document().unwrap().get("a"),
                Some(&Bson::Document(shape.clone())),
                "{shape}"
            );
        }
    });
}

/// New fields are stored in the order of their paths across every operator,
/// not in the order the update wrote them.
#[test]
fn an_update_adds_its_fields_in_path_order() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": 1, "c": 0 }]);
        update(c, doc! {}, doc! { "$set": { "z": 1, "y": 2, "b": 3 } });
        assert_eq!(keys(&one(c)), ["_id", "c", "b", "y", "z"]);

        seed(c, vec![doc! { "_id": 1, "c": 0 }]);
        update(
            c,
            doc! {},
            doc! { "$inc": { "z": 1, "a": 1 }, "$set": { "q": 1, "b": 1 } },
        );
        assert_eq!(keys(&one(c)), ["_id", "c", "a", "b", "q", "z"]);

        // A field the document has keeps its place.
        seed(c, vec![doc! { "_id": 1, "c": 0, "z": 5, "b": 5 }]);
        update(
            c,
            doc! {},
            doc! { "$set": { "z": 1, "y": 2, "b": 3, "a": 4 } },
        );
        assert_eq!(keys(&one(c)), ["_id", "c", "z", "b", "a", "y"]);

        seed(c, vec![doc! { "_id": 1, "o": { "c": 0 } }]);
        update(c, doc! {}, doc! { "$set": { "o.z": 1, "o.b": 2 } });
        assert_eq!(keys(one(c).get_document("o").unwrap()), ["c", "b", "z"]);

        seed(c, vec![doc! { "_id": 1, "c": 0 }]);
        update(
            c,
            doc! {},
            doc! { "$rename": { "c": "zz" }, "$set": { "aa": 1 } },
        );
        assert_eq!(keys(&one(c)), ["_id", "aa", "zz"]);
    });
}

/// `$` is one array position for the whole update: the one the last predicate
/// to walk an array matched, in mongod's order of predicates.
#[test]
fn the_positional_operator_names_one_position() {
    with_wt(|c| {
        let two =
            doc! { "_id": 1, "a": [1, 2, 3, 2], "b": [5, 6, 7, 8], "o": [{ "k": 1 }, { "k": 2 }] };
        let cases: [(Document, Document, &str, Bson); 9] = [
            (
                doc! { "a": 2 },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 9, 3, 2]),
            ),
            // `b` sorts after `a`, so `b: 5` (position 0) is the one that counts.
            (
                doc! { "a": 3, "b": 5 },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([9, 2, 3, 2]),
            ),
            (
                doc! { "b": 8, "a": 1 },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 2, 3, 9]),
            ),
            // The position applies to any array, not only the one queried.
            (
                doc! { "a": 2 },
                doc! { "$set": { "b.$": 9 } },
                "b",
                bson::bson!([5, 9, 7, 8]),
            ),
            (
                doc! { "a": 3, "o.k": 2 },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 9, 3, 2]),
            ),
            // An `$or` of equalities on one path is an `$in`.
            (
                doc! { "$or": [{ "a": 3 }, { "a": 2 }] },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 9, 3, 2]),
            ),
            (
                doc! { "$and": [{ "a": 3 }, { "a": 2 }] },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 9, 3, 2]),
            ),
            (
                doc! { "a": { "$lt": 3, "$gt": 1 } },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([1, 9, 3, 2]),
            ),
            (
                doc! { "a": { "$exists": true } },
                doc! { "$set": { "a.$": 9 } },
                "a",
                bson::bson!([9, 2, 3, 2]),
            ),
        ];
        for (q, u, field, want) in cases {
            seed(c, vec![two.clone()]);
            assert_eq!(applied(&update(c, q.clone(), u.clone())), (1, 1), "{q} {u}");
            assert_eq!(one(c).get(field), Some(&want), "{q} {u}");
        }

        // It is written into the path as text, so it can create a field.
        seed(c, vec![two.clone()]);
        update(c, doc! { "a": 2 }, doc! { "$set": { "x.$": 9 } });
        assert_eq!(one(c).get_document("x"), Ok(&doc! { "1": 9 }));

        let no_match =
            format!("{EXEC}The positional operator did not find the match needed from the query.");
        for q in [
            doc! { "$or": [{ "a": 3 }, { "b": 6 }] },
            doc! { "a.1": 2 },
            doc! { "a": { "$size": 4 } },
            doc! { "a": [1, 2, 3, 2] },
            doc! { "a": { "$not": { "$gt": 5 } } },
            doc! {},
        ] {
            seed(c, vec![two.clone()]);
            let reply = update(c, q.clone(), doc! { "$set": { "a.$": 9 } });
            assert_eq!(write_error(&reply), (2, no_match.clone()), "{q}");
        }
        // An upsert has no element to name.
        seed(c, vec![]);
        let reply = statement(
            c,
            doc! { "q": { "l": 1 }, "u": { "$set": { "l.$": 2 } }, "upsert": true },
        );
        assert_eq!(write_error(&reply), (2, no_match));
        assert!(stored(c).is_empty());

        // findAndModify resolves it the same way.
        seed(c, vec![doc! { "_id": 1, "l": [1, 2, 3] }]);
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": { "l": 2 }, "update": { "$set": { "l.$": 9 } }, "new": true },
            c,
        );
        assert_eq!(
            reply.get_document("value"),
            Ok(&doc! { "_id": 1, "l": [1, 9, 3] }),
            "{reply}"
        );
    });
}

#[test]
fn where_a_positional_element_may_stand() {
    with_wt(|c| {
        let d = doc! { "_id": 1, "a": [1, 2], "n": 7, "o": [{ "t": [1] }] };
        let cases: [(Document, i32, String); 9] = [
            (doc! { "$set": { "$": 9 } }, 2, "Cannot have positional (i.e. '$') element in the first position in path '$'".into()),
            (doc! { "$set": { "$[]": 9 } }, 2, "Cannot have array filter identifier (i.e. '$[<id>]') element in the first position in path '$[]'".into()),
            (doc! { "$set": { "o.$.t.$": 9 } }, 2, "Too many positional (i.e. '$') elements found in path 'o.$.t.$'".into()),
            (doc! { "$set": { "n.$[]": 9 } }, 2, format!("{EXEC}Cannot apply array updates to non-array element n: 7")),
            (doc! { "$set": { "nope.$[]": 9 } }, 2, format!("{EXEC}The path 'nope' must exist in the document in order to apply array updates.")),
            (doc! { "$set": { "a.$[].x": 9 } }, 28, format!("{EXEC}Cannot create field 'x' in element {{0: 1}}")),
            (doc! { "$set": { "a.$[]x": 9 } }, 28, format!("{EXEC}Cannot create field '$[]x' in element {{a: [ 1, 2 ]}}")),
            (doc! { "$set": { "n.x": 9 } }, 28, format!("{EXEC}Cannot create field 'x' in element {{n: 7}}")),
            (doc! { "$rename": { "n": "a.x" } }, 2, format!("{EXEC}The destination field cannot be an array element, 'a.x' in doc with _id: 1 has an array field called 'a'")),
        ];
        for (u, code, message) in cases {
            seed(c, vec![d.clone()]);
            assert_eq!(
                write_error(&update(c, doc! {}, u.clone())),
                (code, message),
                "{u}"
            );
            assert_eq!(one(c), d, "{u}");
        }
    });
}

/// The `_id` may be written so long as it comes out the same.
#[test]
fn the_id_may_be_written_but_not_changed() {
    with_wt(|c| {
        for u in [
            doc! { "$inc": { "_id": 0 } },
            doc! { "$mul": { "_id": 1 } },
            doc! { "$max": { "_id": 0 } },
            doc! { "$set": { "_id": 1 } },
            doc! { "$setOnInsert": { "_id": 2 } },
        ] {
            seed(c, vec![doc! { "_id": 1, "a": 1 }]);
            assert_eq!(applied(&update(c, doc! {}, u.clone())), (1, 0), "{u}");
        }
        let on_path = |p: &str| {
            format!("{EXEC}Performing an update on the path '{p}' would modify the immutable field '_id'")
        };
        for (u, message) in [
            (doc! { "$set": { "_id": 2 } }, on_path("_id")),
            (doc! { "$set": { "_id": 1.0 } }, on_path("_id")),
            (doc! { "$inc": { "_id": 1 } }, on_path("_id")),
            (doc! { "$unset": { "_id": "" } }, on_path("_id")),
            (doc! { "$rename": { "a": "_id" } }, on_path("_id")),
        ] {
            seed(c, vec![doc! { "_id": 1, "a": 1 }]);
            assert_eq!(
                write_error(&update(c, doc! {}, u.clone())),
                (66, message),
                "{u}"
            );
        }

        let nested = doc! { "_id": { "x": 1, "y": 2 }, "a": 1 };
        seed(c, vec![nested.clone()]);
        assert_eq!(
            applied(&update(c, doc! {}, doc! { "$set": { "_id.x": 1 } })),
            (1, 0)
        );
        for (u, message) in [
            (doc! { "$set": { "_id.x": 2 } }, on_path("_id.x")),
            // The same fields in another order are another `_id`.
            (doc! { "$set": { "_id": { "y": 2, "x": 1 } } }, on_path("_id")),
            (
                doc! { "$set": { "_id.z": 2 } },
                format!("{EXEC}Updating the path '_id' to _id: {{ x: 1, y: 2, z: 2 }} would modify the immutable field '_id'"),
            ),
        ] {
            seed(c, vec![nested.clone()]);
            assert_eq!(write_error(&update(c, doc! {}, u.clone())), (66, message), "{u}");
        }
        seed(c, vec![nested]);
        let reply = update(c, doc! {}, doc! { "_id": { "y": 2, "x": 1 }, "a": 1 });
        assert_eq!(
            write_error(&reply),
            (66, format!("{EXEC}After applying the update, the (immutable) field '_id' was found to have been altered to _id: {{ y: 2, x: 1 }}"))
        );

        // A replacement may give the same value in another type, and it is
        // stored as given.
        seed(c, vec![doc! { "_id": 1, "a": 1 }]);
        assert_eq!(
            applied(&update(c, doc! {}, doc! { "_id": 1.0, "z": 1 })),
            (1, 1)
        );
        assert_eq!(one(c), doc! { "_id": 1.0, "z": 1 });

        // A pipeline may not change it either. It did.
        seed(c, vec![doc! { "_id": 1, "a": 1 }]);
        let reply = update(c, doc! {}, vec![doc! { "$set": { "_id": 2 } }]);
        assert_eq!(
            write_error(&reply),
            (66, format!("{EXEC}After applying the update, the (immutable) field '_id' was found to have been altered to _id: 2"))
        );
        assert_eq!(one(c), doc! { "_id": 1, "a": 1 });
    });
}

#[test]
fn what_the_array_operators_store() {
    with_wt(|c| {
        let d = doc! {
            "_id": 1,
            "a": [3, 1, 2],
            "o": [{ "k": 2, "v": "x" }, { "k": 1, "v": "y" }],
            "i": 5,
            "l": 5_i64,
        };
        let cases: [(Document, &str, Bson); 9] = [
            // A whole double is an integer here.
            (
                doc! { "$push": { "a": { "$each": [7], "$position": 1.0 } } },
                "a",
                bson::bson!([3, 7, 1, 2]),
            ),
            (
                doc! { "$push": { "a": { "$each": [7], "$slice": 2.0 } } },
                "a",
                bson::bson!([3, 1]),
            ),
            // Elements that are not documents have no field to sort by.
            (
                doc! { "$push": { "a": { "$each": [0], "$sort": { "x": 1 } } } },
                "a",
                bson::bson!([3, 1, 2, 0]),
            ),
            // Only a document that OPENS with `$each` is the modifier form.
            (
                doc! { "$addToSet": { "a": { "x": 1, "$each": [9] } } },
                "a",
                bson::bson!([3, 1, 2, { "x": 1, "$each": [9] }]),
            ),
            (
                doc! { "$pull": { "o": { "$or": [{ "k": 1 }, { "v": "x" }] } } },
                "o",
                bson::bson!([]),
            ),
            // The same fields in another order are another document.
            (
                doc! { "$pullAll": { "o": [{ "v": "x", "k": 2 }] } },
                "o",
                bson::bson!([{ "k": 2, "v": "x" }, { "k": 1, "v": "y" }]),
            ),
            // A long on either side of `$bit` makes the result a long.
            (
                doc! { "$bit": { "i": { "and": 3_i64 } } },
                "i",
                Bson::Int64(1),
            ),
            (doc! { "$bit": { "l": { "or": 3 } } }, "l", Bson::Int64(7)),
            (doc! { "$bit": { "i": { "xor": 3 } } }, "i", Bson::Int32(6)),
        ];
        for (u, field, want) in cases {
            seed(c, vec![d.clone()]);
            let reply = update(c, doc! {}, u.clone());
            assert!(!reply.contains_key("writeErrors"), "{u}: {reply}");
            assert_eq!(one(c).get(field), Some(&want), "{u}");
        }

        let refused: [(Document, i32, &str); 12] = [
            (doc! { "$push": { "a": { "$each": [7], "x": 1 } } }, 2, "Unrecognized clause in $push: x"),
            (doc! { "$push": { "a": { "$each": [7], "$position": 1.5 } } }, 2, "The value for $position must be an integer value, not of type: double"),
            (doc! { "$push": { "a": { "$each": [7], "$slice": "2" } } }, 2, "The value for $slice must be an integer value but was given type: string"),
            (doc! { "$push": { "a": { "$each": [0], "$sort": 2 } } }, 2, "The $sort element value must be either 1 or -1"),
            (doc! { "$push": { "a": { "$each": [0], "$sort": "1" } } }, 2, "The $sort is invalid: use 1/-1 to sort the whole element, or {field:1/-1} to sort embedded fields"),
            (doc! { "$push": { "a": { "$each": [0], "$sort": {} } } }, 2, "The sort pattern is empty when it should be a set of fields."),
            (doc! { "$push": { "o": { "$each": [], "$sort": { "k": 2 } } } }, 2, "The sort element value must be either 1 or -1"),
            (doc! { "$push": { "o": { "$each": [], "$sort": { "": 1 } } } }, 2, "The sort field cannot be empty"),
            (doc! { "$addToSet": { "a": { "$each": [9], "$position": 0 } } }, 2, "Found unexpected fields after $each in $addToSet: { $each: [ 9 ], $position: 0 }"),
            (doc! { "$pullAll": { "a": 1 } }, 2, "$pullAll requires an array argument but was given a int"),
            (doc! { "$unset": "i" }, 9, "Modifiers operate on fields but we found type string instead. For example: {$mod: {<field>: ...}} not {$unset: \"i\"}"),
            (doc! { "$rename": { "o": "o.o" } }, 2, "The source and target field for $rename must not be on the same path: o: \"o.o\""),
        ];
        for (u, code, message) in refused {
            seed(c, vec![d.clone()]);
            assert_eq!(
                write_error(&update(c, doc! {}, u.clone())),
                (code, message.to_string()),
                "{u}"
            );
            assert_eq!(one(c), d, "{u}");
            // Refused when the update is read: with nothing to match, too.
            seed(c, vec![]);
            dispatch_full(&doc! { "create": "c" }, c);
            assert_eq!(
                write_error(&update(c, doc! {}, u.clone())),
                (code, message.to_string()),
                "{u}"
            );
        }
    });
}

/// `nModified` where the stored bytes do not decide it.
#[test]
fn what_counts_as_a_modification() {
    with_wt(|c| {
        let d = doc! { "_id": 1, "s": [1, 2, 3], "e": [] };
        // A replacement that leaves out the `_id` is never the stored document.
        seed(c, vec![d.clone()]);
        assert_eq!(
            applied(&update(c, doc! {}, doc! { "s": [1, 2, 3], "e": [] })),
            (1, 1)
        );
        assert_eq!(applied(&update(c, doc! {}, d.clone())), (1, 0));
        // A `$sort` that ran over something counts, whatever it moved.
        assert_eq!(
            applied(&update(
                c,
                doc! {},
                doc! { "$push": { "s": { "$each": [], "$sort": 1 } } }
            )),
            (1, 1)
        );
        assert_eq!(
            applied(&update(
                c,
                doc! {},
                doc! { "$push": { "e": { "$each": [], "$sort": 1 } } }
            )),
            (1, 0)
        );
        assert_eq!(
            applied(&update(
                c,
                doc! {},
                doc! { "$push": { "s": { "$each": [], "$slice": 5 } } }
            )),
            (1, 0)
        );
        assert_eq!(one(c), d);
    });
}

#[test]
fn what_a_document_may_not_hold() {
    with_wt(|c| {
        let d = doc! { "_id": 1, "o": {}, "s": 1 };
        let dollar = |f: &str| {
            format!("{EXEC}The dollar ($) prefixed field '{f}' in '{f}' is not allowed in the context of an update's replacement document. Consider using an aggregation pipeline with $replaceWith.")
        };
        for (u, code, message) in [
            (doc! { "$set": { "$x": 1 } }, 52, dollar("$x")),
            (doc! { "$inc": { "$x.y": 1 } }, 52, dollar("$x")),
            (doc! { "$rename": { "s": "$z" } }, 52, dollar("$z")),
            (
                doc! { "$set": { "$id": 1 } },
                55,
                format!("{EXEC}Found $id field without a $ref before it, which is invalid."),
            ),
            (
                doc! { "$set": { "$ref": "c" } },
                55,
                format!("{EXEC}The DBRef $ref field must be followed by a $id field"),
            ),
            (
                doc! { "$set": { "$ref": 1 } },
                55,
                format!("{EXEC}The DBRef $ref field must be a String, not a int"),
            ),
            (
                doc! { "$set": { "$db": "d" } },
                55,
                format!("{EXEC}Found $db field without a $id before it, which is invalid."),
            ),
        ] {
            seed(c, vec![d.clone()]);
            assert_eq!(
                write_error(&update(c, doc! {}, u.clone())),
                (code, message),
                "{u}"
            );
            assert_eq!(one(c), d, "{u}");
        }
        // Below the top level it may; and an upsert's new document may.
        seed(c, vec![d.clone()]);
        assert_eq!(
            applied(&update(c, doc! {}, doc! { "$set": { "o.$x": 1 } })),
            (1, 1)
        );
        let reply = statement(
            c,
            doc! { "q": { "_id": 5 }, "u": { "$set": { "$x": 1 } }, "upsert": true },
        );
        assert!(!reply.contains_key("writeErrors"), "{reply}");
        assert_eq!(stored(c)[1], doc! { "_id": 5, "$x": 1 });

        // 180 levels is the most a document nests.
        let path = |n: usize| vec!["a"; n].join(".");
        seed(c, vec![doc! { "_id": 1 }]);
        let mut at_limit = Document::new();
        at_limit.insert(path(180), 1);
        assert_eq!(
            applied(&update(c, doc! {}, doc! { "$set": at_limit })),
            (1, 1)
        );
        for (n, value) in [
            (181, bson::bson!(1)),
            (179, bson::bson!({ "b": { "c": 1 } })),
            (179, bson::bson!([[1]])),
        ] {
            seed(c, vec![doc! { "_id": 1 }]);
            let mut too_deep = Document::new();
            too_deep.insert(path(n), value);
            assert_eq!(
                write_error(&update(c, doc! {}, doc! { "$set": too_deep })),
                (
                    15,
                    format!("{EXEC}Document exceeds maximum nesting depth of 180")
                )
            );
            assert_eq!(one(c), doc! { "_id": 1 });
        }
    });
}

#[test]
fn a_pipeline_update_is_checked_and_follows_the_aggregation_rules() {
    with_wt(|c| {
        let d = doc! { "_id": 1, "a": 5, "b": { "c": 2 }, "arr": [1, 2] };
        // A path through a scalar replaces it, a path through an array is given
        // to every element, and a nested document is more specification.
        for (stage, field, want) in [
            (doc! { "$set": { "a.b": 1 } }, "a", bson::bson!({ "b": 1 })),
            (
                doc! { "$set": { "arr.0": 9 } },
                "arr",
                bson::bson!([{ "0": 9 }, { "0": 9 }]),
            ),
            (
                doc! { "$set": { "b": { "d": 3 } } },
                "b",
                bson::bson!({ "c": 2, "d": 3 }),
            ),
            (
                doc! { "$set": { "x": { "$const": 1 } } },
                "x",
                bson::bson!(1),
            ),
        ] {
            seed(c, vec![d.clone()]);
            assert_eq!(
                applied(&update(c, doc! {}, vec![stage.clone()])),
                (1, 1),
                "{stage}"
            );
            assert_eq!(one(c).get(field), Some(&want), "{stage}");
        }
        for (stage, code, message) in [
            (doc! { "$match": { "a": 5 } }, 72, "$match is not allowed to be used within an update".to_string()),
            (doc! { "$set": 1 }, 40272, "$set specification stage must be an object, got int".to_string()),
            (doc! { "$set": { "a": { "$nope": 1 } } }, 168, "Invalid $set :: caused by :: Unrecognized expression '$nope'".to_string()),
            (
                doc! { "$set": { "$a": 1 } },
                16410,
                "Invalid $set :: caused by :: Consider using $getField or $setField for a field path with '.' or '$'. :: caused by :: FieldPath field names may not start with '$', given '$a'.".to_string(),
            ),
            (doc! { "$set": { "a": { "$divide": [1, 0] } } }, 2, format!("{EXEC}can't $divide by zero")),
        ] {
            seed(c, vec![d.clone()]);
            assert_eq!(write_error(&update(c, doc! {}, vec![stage.clone()])), (code, message), "{stage}");
            assert_eq!(one(c), d, "{stage}");
        }
        seed(c, vec![d.clone()]);
        let reply = statement(
            c,
            doc! { "q": {}, "u": [{ "$set": { "z": 1 } }], "arrayFilters": [{ "x": 1 }] },
        );
        assert_eq!(
            write_error(&reply),
            (
                9,
                "arrayFilters may not be specified for pipeline-style updates".to_string()
            )
        );
        assert_eq!(
            refusal(&update(c, doc! {}, vec![doc! { "$nope": 1 }])),
            (
                40324,
                "Unrecognized pipeline stage name: '$nope'".to_string()
            )
        );
        assert_eq!(
            refusal(&update(c, doc! {}, vec![doc! {}])),
            (
                40323,
                "A pipeline stage specification object must contain exactly one field.".to_string()
            )
        );
        assert_eq!(one(c), d);
    });
}

#[test]
fn what_an_upsert_takes_from_its_query() {
    with_wt(|c| {
        seed(c, vec![]);
        let upsert = |c: &mut CommandContext, q: Document| {
            statement(
                c,
                doc! { "q": q, "u": { "$set": { "z": 1 } }, "upsert": true },
            )
        };
        assert_eq!(
            write_error(&upsert(c, doc! { "a": 1, "a.b": 2 })),
            (
                54,
                format!(
                    "{EXEC}cannot infer query fields to set, both paths 'a.b' and 'a' are matched"
                )
            )
        );
        assert_eq!(
            write_error(&upsert(c, doc! { "a.b": 2, "a": 1 })),
            (
                54,
                format!(
                    "{EXEC}cannot infer query fields to set, both paths 'a.b' and 'a' are matched"
                )
            )
        );
        assert_eq!(
            write_error(&upsert(c, doc! { "$expr": { "$eq": ["$a", 1] } })),
            (
                224,
                "$expr is not allowed in the query predicate for an upsert".to_string()
            )
        );
        assert!(stored(c).is_empty());
        // A test of the elements is not a value for the field.
        let reply = upsert(
            c,
            doc! { "_id": 1, "a": { "$all": [{ "$elemMatch": { "b": 1 } }] } },
        );
        assert!(!reply.contains_key("writeErrors"), "{reply}");
        assert_eq!(one(c), doc! { "_id": 1, "z": 1 });
    });
}

#[test]
fn what_the_update_command_takes() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": 0 }]);
        let stmt = doc! { "q": { "_id": 0 }, "u": { "$set": { "x": 1 } } };
        let with = |extra: Document| {
            let mut command = doc! { "update": "c", "updates": [stmt.clone()] };
            command.extend(extra);
            command
        };
        for (command, code, message) in [
            (
                with(doc! { "nope": 1 }),
                40415,
                "BSON field 'update.nope' is an unknown field.",
            ),
            (
                with(doc! { "multi": true }),
                40415,
                "BSON field 'update.multi' is an unknown field.",
            ),
            (
                with(doc! { "writeConcern": { "nope": 1 } }),
                40415,
                "BSON field 'WriteConcernOptions.nope' is an unknown field.",
            ),
            (
                doc! { "update": "c", "updates": 1 },
                14,
                "BSON field 'update.updates' is the wrong type 'int', expected type 'array'",
            ),
            (
                doc! { "update": "c" },
                40414,
                "BSON field 'update.updates' is missing but a required field",
            ),
            (
                doc! { "update": "c", "updates": [1] },
                14,
                "BSON field 'update.updates.0' is the wrong type 'int', expected type 'object'",
            ),
            (
                doc! { "update": "c", "updates": [{ "q": {}, "u": { "$set": { "x": 1 } }, "limit": 1 }] },
                40415,
                "BSON field 'update.updates.limit' is an unknown field.",
            ),
            (
                doc! { "update": "c", "updates": [{ "q": {}, "u": { "$set": { "x": 1 } }, "upsert": 1 }] },
                14,
                "BSON field 'update.updates.upsert' is the wrong type 'int', expected type 'bool'",
            ),
            (
                doc! { "update": "c", "updates": [{ "q": Bson::Null, "u": { "$set": { "x": 1 } } }] },
                40414,
                "BSON field 'update.updates.q' is missing but a required field",
            ),
        ] {
            assert_eq!(
                refusal(&dispatch_full(&command, c)),
                (code, message.to_string()),
                "{command}"
            );
        }
        assert_eq!(one(c), doc! { "_id": 0 });

        // `let` is checked against each statement.
        let reply = dispatch_full(&with(doc! { "let": { "ROOT": 1 } }), c);
        assert_eq!(
            write_error(&reply),
            (9, "Invalid 'let' parameter :: caused by :: 'ROOT' starts with an invalid character for a user variable name".to_string())
        );
        let reply = dispatch_full(&with(doc! { "let": { "v": "$a" } }), c);
        assert_eq!(write_error(&reply).0, 4890500);

        // An empty hint is no hint.
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{ "q": { "_id": 0 }, "u": { "$set": { "x": 1 } }, "hint": {} }] },
            c,
        );
        assert_eq!(applied(&reply), (1, 1));

        // An update that cannot insert does not create its collection.
        let reply = dispatch_full(
            &doc! { "update": "absent", "updates": [{ "q": {}, "u": { "$set": { "x": 1 } }, "multi": true }] },
            c,
        );
        assert_eq!(applied(&reply), (0, 0));
        let listed = dispatch_full(&doc! { "listCollections": 1, "nameOnly": true }, c);
        let batch = listed
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(batch.len(), 1, "{listed}");
    });
}

#[test]
fn what_find_and_modify_takes() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": 0, "l": [1, 2, 3] }]);
        let fam = |extra: Document| {
            let mut command = doc! { "findAndModify": "c", "query": { "_id": 0 } };
            command.extend(extra);
            command
        };
        let set = doc! { "$set": { "x": 1 } };
        for (command, code, message) in [
            (fam(doc! { "update": set.clone(), "nope": 1 }), 40415, "BSON field 'findAndModify.nope' is an unknown field."),
            (fam(doc! { "update": set.clone(), "multi": true }), 40415, "BSON field 'findAndModify.multi' is an unknown field."),
            (fam(doc! { "remove": true, "arrayFilters": [{ "e": 1 }] }), 9, "Cannot specify arrayFilters and remove=true"),
            (fam(doc! { "update": [{ "$set": { "x": 1 } }], "arrayFilters": [{ "e": 1 }] }), 9, "Cannot specify arrayFilters and a pipeline update"),
            (fam(doc! { "update": [{ "$nope": 1 }] }), 40324, "Unrecognized pipeline stage name: '$nope'"),
            (fam(doc! { "update": [1] }), 14, "Each element of the 'pipeline' array must be an object"),
            (
                fam(doc! { "update": set.clone(), "fields": { "$v": 1 } }),
                16410,
                "Consider using $getField or $setField for a field path with '.' or '$'. :: caused by :: FieldPath field names may not start with '$', given '$v'.",
            ),
            (fam(doc! { "update": set.clone(), "fields": { "v": { "$nope": 1 } } }), 31325, "Unknown expression $nope"),
            (fam(doc! { "update": set.clone(), "fields": { "l": 1, "x": 0 } }), 31254, "Cannot do exclusion on field x in inclusion projection"),
            (
                fam(doc! { "update": set.clone(), "new": true, "fields": { "l.$": 1 } }),
                2,
                "cannot use a positional projection and return the new document",
            ),
            (
                fam(doc! { "update": set.clone(), "bypassDocumentValidation": "x" }),
                14,
                "BSON field 'findAndModify.bypassDocumentValidation' is the wrong type 'string', expected types '[bool, decimal, double, int, long]'",
            ),
        ] {
            let reply = dispatch_full(&command, c);
            let (got_code, got_message) = refusal(&reply);
            // The expected-type list is a set, and mongod orders it per build.
            if code == 14 && message.contains("expected types") {
                assert_eq!(got_code, 14, "{command}");
                assert!(got_message.starts_with("BSON field 'findAndModify.bypassDocumentValidation'"));
            } else {
                assert_eq!((got_code, got_message), (code, message.to_string()), "{command}");
            }
        }
        assert_eq!(one(c), doc! { "_id": 0, "l": [1, 2, 3] });

        // A number is a bool here: `upsert: 1` inserts.
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": { "_id": 9 }, "update": set.clone(), "upsert": 1 },
            c,
        );
        assert_eq!(
            reply.get_document("lastErrorObject"),
            Ok(&doc! { "n": 1, "updatedExisting": false, "upserted": 9 }),
            "{reply}"
        );
        // A null query is no query.
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": Bson::Null, "update": set, "sort": { "_id": 1 } },
            c,
        );
        assert_eq!(
            reply.get_document("value"),
            Ok(&doc! { "_id": 0, "l": [1, 2, 3] }),
            "{reply}"
        );
        // `system.views` is not a view.
        dispatch_full(&doc! { "create": "v", "viewOn": "c", "pipeline": [] }, c);
        for command in [
            doc! { "findAndModify": "system.views", "query": {}, "remove": true },
            doc! { "update": "system.views", "updates": [{ "q": {}, "u": { "$set": { "x": 1 } } }] },
        ] {
            let (code, message) = refusal(&dispatch_full(&command, c));
            assert_eq!(code, 73, "{command}");
            assert!(message.starts_with("cannot write to "), "{message}");
        }
    });
}

/// An empty timestamp is filled in a document given whole, at its top level,
/// and nowhere else.
#[test]
fn an_empty_timestamp_is_filled_in_a_whole_document() {
    with_wt(|c| {
        let empty = Bson::Timestamp(bson::Timestamp {
            time: 0,
            increment: 0,
        });
        let filled = |d: &Document, field: &str| matches!(d.get(field), Some(Bson::Timestamp(t)) if t.time != 0);
        seed(
            c,
            vec![doc! { "_id": 1, "a": empty.clone(), "b": { "t": empty.clone() } }],
        );
        let d = one(c);
        assert!(filled(&d, "a"), "{d}");
        assert_eq!(d.get_document("b").unwrap().get("t"), Some(&empty));

        update(c, doc! {}, doc! { "$set": { "s": empty.clone() } });
        assert_eq!(one(c).get("s"), Some(&empty));

        update(c, doc! {}, doc! { "r": empty.clone() });
        let d = one(c);
        assert!(filled(&d, "r"), "{d}");

        seed(c, vec![]);
        statement(
            c,
            doc! { "q": { "_id": 2 }, "u": { "u": empty.clone() }, "upsert": true },
        );
        let d = one(c);
        assert!(filled(&d, "u"), "{d}");
    });
}
