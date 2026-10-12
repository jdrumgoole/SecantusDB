//! `insert`, `delete`, `count` and `distinct` over real WiredTiger: what they
//! store and remove, what they accept and refuse. Every expectation is what
//! mongod 8.2.11 answered to the same command
//! (`tools/probes/insert_delete_count_distinct.py`).

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

fn index(c: &mut CommandContext, key: Document, name: &str, unique: bool) {
    let reply = dispatch_full(
        &doc! { "createIndexes": "c", "indexes": [{ "key": key, "name": name, "unique": unique }] },
        c,
    );
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
}

/// The documents the collection holds, in the order it holds them.
fn stored(c: &mut CommandContext) -> Vec<Document> {
    let reply = dispatch_full(&doc! { "find": "c" }, c);
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn ids(docs: &[Document]) -> Vec<i32> {
    docs.iter().map(|d| d.get_i32("_id").unwrap()).collect()
}

fn found(c: &mut CommandContext, filter: Document) -> Vec<i32> {
    let reply = dispatch_full(&doc! { "find": "c", "filter": filter }, c);
    let batch = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap();
    batch
        .iter()
        .map(|d| d.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

fn insert(c: &mut CommandContext, docs: Vec<Bson>) -> Document {
    dispatch_full(&doc! { "insert": "c", "documents": docs }, c)
}

fn insert_one(c: &mut CommandContext, d: Document) -> Document {
    insert(c, vec![Bson::Document(d)])
}

fn delete(c: &mut CommandContext, statement: Document) -> Document {
    dispatch_full(&doc! { "delete": "c", "deletes": [statement] }, c)
}

/// `n` of a write with no write error.
fn applied(reply: &Document) -> i32 {
    assert!(!reply.contains_key("writeErrors"), "{reply}");
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    reply.get_i32("n").unwrap()
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

fn rows() -> Vec<Document> {
    vec![
        doc! { "_id": 1, "a": 3, "s": "x" },
        doc! { "_id": 2, "a": 1, "s": "X" },
        doc! { "_id": 3, "a": 2, "s": "y" },
        doc! { "_id": 4, "a": 1, "s": "x" },
        doc! { "_id": 5, "s": "z" },
    ]
}

/// Two `_id` documents that differ only in how a number is written are one
/// `_id`. The `_id` index keys a document by its bytes, so both were stored.
#[test]
fn a_document_id_is_unique_by_value() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": { "a": 1, "b": { "c": [1, 2.5] } } }]);
        for same in [
            doc! { "a": 1.0, "b": { "c": [1, 2.5] } },
            doc! { "a": 1_i64, "b": { "c": [1.0, 2.5] } },
            doc! { "a": 1, "b": { "c": [1_i64, 2.5] } },
        ] {
            let reply = insert_one(c, doc! { "_id": same.clone() });
            let (code, errmsg) = write_error(&reply);
            assert_eq!(code, 11000, "{same}");
            assert!(errmsg.contains("index: _id_ dup key: { _id: {"), "{errmsg}");
            assert_eq!(reply.get_i32("n"), Ok(0));
        }
        // A different value, a different field order and a different type
        // are other documents.
        for other in [
            doc! { "a": 1, "b": { "c": [1, 2.5, 3] } },
            doc! { "b": { "c": [1, 2.5] }, "a": 1 },
            doc! { "a": 1, "b": { "c": [1, "2.5"] } },
        ] {
            assert_eq!(applied(&insert_one(c, doc! { "_id": other })), 1);
        }
        assert_eq!(stored(c).len(), 4);
    });
}

/// The claim on a document `_id` goes with the document: once it is deleted
/// the value can be inserted again, written either way.
#[test]
fn a_deleted_document_id_can_be_inserted_again() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": { "a": 1 } }]);
        let gone = delete(c, doc! { "q": { "_id": { "a": 1.0 } }, "limit": 0 });
        assert_eq!(applied(&gone), 1);
        assert_eq!(applied(&insert_one(c, doc! { "_id": { "a": 1.0 } })), 1);
        let (code, _) = write_error(&insert_one(c, doc! { "_id": { "a": 1 } }));
        assert_eq!(code, 11000);
        assert_eq!(stored(c), vec![doc! { "_id": { "a": 1.0 } }]);
    });
}

/// In one unordered batch each value is stored once, and an upsert finds the
/// document whichever way the number is written.
#[test]
fn a_document_id_is_unique_within_a_batch_and_across_an_upsert() {
    with_wt(|c| {
        seed(c, vec![]);
        let reply = dispatch_full(
            &doc! {
                "insert": "c",
                "ordered": false,
                "documents": [
                    { "_id": { "a": 0.0 } },
                    { "_id": { "a": -0.0 } },
                    { "_id": { "a": 0 } },
                    { "_id": { "a": 1 } },
                ],
            },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(2), "{reply}");
        assert_eq!(reply.get_array("writeErrors").unwrap().len(), 2);
        let upsert = dispatch_full(
            &doc! {
                "update": "c",
                "updates": [{ "q": { "_id": { "a": 1_i64 } }, "u": { "$set": { "x": 1 } }, "upsert": true }],
            },
            c,
        );
        assert_eq!(upsert.get_i32("n"), Ok(1), "{upsert}");
        assert!(!upsert.contains_key("upserted"), "{upsert}");
        assert_eq!(stored(c).len(), 2);
    });
}

/// A duplicate `_id` is the error reported when a unique index is repeated
/// as well, and the key is printed as mongod prints it.
#[test]
fn a_duplicate_key_names_the_id_first_and_prints_the_value() {
    with_wt(|c| {
        seed(c, vec![doc! { "_id": 1, "a": 1 }]);
        index(c, doc! { "a": 1 }, "a_1", true);
        let (_, errmsg) = write_error(&insert_one(c, doc! { "_id": 1, "a": 1 }));
        assert!(
            errmsg.ends_with("index: _id_ dup key: { _id: 1 }"),
            "{errmsg}"
        );

        let date = bson::DateTime::from_millis(1577934245000);
        let shown = [
            (Bson::DateTime(date), "new Date(1577934245000)"),
            (
                Bson::Document(doc! { "a": 1.0, "b": ["x"] }),
                "{ a: 1.0, b: [ \"x\" ] }",
            ),
            (
                Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes: vec![0xab, 0xcd],
                }),
                "BinData(0, ABCD)",
            ),
            (
                Bson::Timestamp(bson::Timestamp {
                    time: 5,
                    increment: 6,
                }),
                "Timestamp(5, 6)",
            ),
            (Bson::Double(1e-7), "1e-07"),
            (Bson::Double(1e20), "1e+20"),
            (Bson::Array(vec![]), "undefined"),
        ];
        for (value, text) in shown {
            seed(c, vec![doc! { "_id": 1, "a": value.clone() }]);
            index(c, doc! { "a": 1 }, "a_1", true);
            let (code, errmsg) = write_error(&insert_one(c, doc! { "_id": 2, "a": value }));
            assert_eq!(code, 11000);
            assert!(
                errmsg.ends_with(&format!("dup key: {{ a: {text} }}")),
                "{errmsg}"
            );
        }
    });
}

/// A `$`-prefixed field anywhere inside an `_id` document is refused, and the
/// field named is the first one met. A DBRef is allowed.
#[test]
fn an_id_holds_no_dollar_field_at_any_depth() {
    with_wt(|c| {
        seed(c, vec![]);
        for (id, field) in [
            (doc! { "a": { "$b": 1 } }, "$b"),
            (doc! { "a": [{ "$b": 1 }] }, "$b"),
            (doc! { "a": { "b": { "$c": 1 } }, "$z": 1 }, "$c"),
            (doc! { "x": 1, "$y": 2, "$z": 3 }, "$y"),
            (doc! { "a": { "$id": 1 } }, "$id"),
        ] {
            let (code, errmsg) = write_error(&insert_one(c, doc! { "_id": id }));
            assert_eq!(code, 52);
            assert_eq!(
                errmsg,
                format!(
                    "_id fields may not contain '$'-prefixed fields: {field} is not valid for storage."
                )
            );
        }
        for id in [
            doc! { "$ref": "x", "$id": 1 },
            doc! { "a": { "$ref": "x", "$id": 1 } },
            doc! { "a.b": 1 },
        ] {
            assert_eq!(applied(&insert_one(c, doc! { "_id": id })), 1);
        }
    });
}

/// A document nests 180 levels and no further.
#[test]
fn an_insert_past_180_levels_is_refused() {
    fn nested(depth: usize) -> Bson {
        (0..depth).fold(Bson::Int32(1), |inner, _| {
            Bson::Document(doc! { "a": inner })
        })
    }
    with_wt(|c| {
        seed(c, vec![]);
        assert_eq!(
            applied(&insert_one(c, doc! { "_id": 1, "a": nested(179) })),
            1
        );
        let (code, errmsg) = write_error(&insert_one(c, doc! { "_id": 2, "a": nested(180) }));
        assert_eq!(
            (code, errmsg.as_str()),
            (
                15,
                "cannot insert document because it exceeds 180 levels of nesting"
            )
        );
        assert_eq!(stored(c).len(), 1);
    });
}

/// What `insert` refuses before it stores anything, and the null it drops.
#[test]
fn insert_checks_its_arguments_before_it_stores() {
    with_wt(|c| {
        seed(c, vec![]);
        let cases = [
            (
                doc! { "insert": "c", "documents": [{ "_id": 1 }, 2] },
                14,
                "BSON field 'insert.documents.1' is the wrong type 'int', expected type 'object'",
            ),
            (
                doc! { "insert": "c", "documents": "x" },
                14,
                "BSON field 'insert.documents' is the wrong type 'string', expected type 'array'",
            ),
            (
                doc! { "insert": "c" },
                40414,
                "BSON field 'insert.documents' is missing but a required field",
            ),
            (
                doc! { "insert": "c", "documents": [{ "_id": 1 }], "nope": 1, "ordered": "x" },
                40415,
                "BSON field 'insert.nope' is an unknown field.",
            ),
            (
                doc! { "insert": "c", "documents": [{ "_id": 1 }], "let": {} },
                40415,
                "BSON field 'insert.let' is an unknown field.",
            ),
            (
                doc! { "insert": "c", "documents": [{ "_id": 1 }], "bypassEmptyTsReplacement": 1 },
                14,
                "Field 'bypassEmptyTsReplacement' should be a boolean value, but found: int",
            ),
            (
                doc! { "insert": "c", "documents": [Bson::Null] },
                16,
                "Write batch sizes must be between 1 and 100000. Got 0 operations.",
            ),
        ];
        for (command, code, errmsg) in cases {
            assert_eq!(
                refusal(&dispatch_full(&command, c)),
                (code, errmsg.to_string()),
                "{command}"
            );
        }
        assert!(stored(c).is_empty());

        let reply = insert(
            c,
            vec![
                doc! { "_id": 1 }.into(),
                Bson::Null,
                doc! { "_id": 2 }.into(),
            ],
        );
        assert_eq!(applied(&reply), 2);
    });
}

/// `bypassEmptyTsReplacement` keeps an empty timestamp as it was sent.
#[test]
fn an_empty_timestamp_is_kept_when_asked() {
    let empty = Bson::Timestamp(bson::Timestamp {
        time: 0,
        increment: 0,
    });
    with_wt(|c| {
        seed(c, vec![]);
        let reply = dispatch_full(
            &doc! {
                "insert": "c",
                "documents": [{ "_id": 1, "t": empty.clone() }],
                "bypassEmptyTsReplacement": true,
            },
            c,
        );
        assert_eq!(applied(&reply), 1);
        assert_eq!(stored(c)[0].get("t"), Some(&empty));

        assert_eq!(
            applied(&insert_one(c, doc! { "_id": 2, "t": empty.clone() })),
            1
        );
        assert_ne!(stored(c)[1].get("t"), Some(&empty));
    });
}

/// A delete statement names its `limit`. One without it removed every
/// matching document, and a statement mongod refuses stopped the command
/// only after the statements ahead of it had run.
#[test]
fn delete_checks_every_statement_before_it_removes() {
    with_wt(|c| {
        let cases = [
            (
                doc! { "delete": "c", "deletes": [{ "q": {} }] },
                40414,
                "BSON field 'delete.deletes.limit' is missing but a required field",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "limit": 0 }] },
                40414,
                "BSON field 'delete.deletes.q' is missing but a required field",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": { "_id": 1 }, "limit": 0 }, { "q": { "_id": 4 }, "limit": 7 }] },
                9,
                "The limit field in delete objects must be 0 or 1. Got 7",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": { "_id": 1 }, "limit": 0 }, 2] },
                14,
                "BSON field 'delete.deletes.1' is the wrong type 'int', expected type 'object'",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 0, "multi": true }] },
                40415,
                "BSON field 'delete.deletes.multi' is an unknown field.",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 0 }], "hint": "x" },
                40415,
                "BSON field 'delete.hint' is an unknown field.",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 1.5 }] },
                9,
                "The limit field in delete objects must be 0 or 1. Got 1.5",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 2147483648_i64 }] },
                9,
                "The limit field in delete objects must be 0 or 1. Got 2.14748e+09",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": f64::NAN }] },
                9,
                "The limit field in delete objects must be 0 or 1. Got nan",
            ),
            (
                doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 1000000 }] },
                9,
                "The limit field in delete objects must be 0 or 1. Got 1e+06",
            ),
        ];
        seed(c, rows());
        for (command, code, errmsg) in cases {
            assert_eq!(
                refusal(&dispatch_full(&command, c)),
                (code, errmsg.to_string()),
                "{command}"
            );
            assert_eq!(stored(c).len(), 5, "{command}");
        }
        // A null among the statements is no statement.
        let reply = dispatch_full(
            &doc! { "delete": "c", "deletes": [{ "q": { "_id": 1 }, "limit": 0 }, Bson::Null] },
            c,
        );
        assert_eq!(applied(&reply), 1);
    });
}

/// `limit: 1` removes the first document the hint reads.
#[test]
fn a_limited_delete_follows_its_hint() {
    with_wt(|c| {
        let all = doc! { "a": { "$gte": 1 } };
        let cases: [(Bson, Vec<i32>); 4] = [
            (doc! { "$natural": 1 }.into(), vec![2, 3, 4, 5]),
            (doc! { "$natural": -1 }.into(), vec![1, 2, 3, 5]),
            (doc! { "a": 1 }.into(), vec![1, 3, 4, 5]),
            ("a_1".into(), vec![1, 3, 4, 5]),
        ];
        for (hint, left) in cases {
            seed(c, rows());
            index(c, doc! { "a": 1 }, "a_1", false);
            let reply = delete(
                c,
                doc! { "q": all.clone(), "limit": 1, "hint": hint.clone() },
            );
            assert_eq!(applied(&reply), 1, "{hint}");
            assert_eq!(ids(&stored(c)), left, "{hint}");
        }
        seed(c, rows());
        let reply = delete(c, doc! { "q": {}, "limit": 1, "hint": { "$natural": -1 } });
        assert_eq!(applied(&reply), 1);
        assert_eq!(ids(&stored(c)), vec![1, 2, 3, 4]);

        // A `$natural` that is not a direction fails its statement.
        let reply = delete(c, doc! { "q": {}, "limit": 0, "hint": { "$natural": 2 } });
        assert_eq!(
            write_error(&reply),
            (
                2,
                "$natural hint cannot be set to a value other than -1 or 1.".to_string()
            )
        );
        assert_eq!(stored(c).len(), 4);
    });
}

/// An `$in` over an index reads the documents in the index's order, whatever
/// order the list is written in.
#[test]
fn an_in_list_reads_in_index_order() {
    with_wt(|c| {
        seed(c, rows());
        index(c, doc! { "s": -1 }, "s_-1", false);
        assert_eq!(found(c, doc! { "s": { "$in": ["x", "y"] } }), vec![3, 1, 4]);
        let reply = delete(c, doc! { "q": { "s": { "$in": ["x", "y"] } }, "limit": 1 });
        assert_eq!(applied(&reply), 1);
        assert_eq!(ids(&stored(c)), vec![1, 2, 4, 5]);

        seed(c, rows());
        index(c, doc! { "s": 1 }, "s_1", false);
        assert_eq!(
            found(c, doc! { "s": { "$in": ["y", "x", "y"] } }),
            vec![1, 4, 3]
        );
        index(c, doc! { "a": -1 }, "a_-1", false);
        assert_eq!(found(c, doc! { "a": { "$in": [1, 3] } }), vec![1, 2, 4]);
    });
}

/// What a delete statement's `let`, `$expr` and `$text` are refused for.
#[test]
fn a_delete_statement_reports_what_its_query_cannot_do() {
    with_wt(|c| {
        seed(c, rows());
        let with_let = |vars: Document| {
            doc! {
                "delete": "c",
                "deletes": [{ "q": { "$expr": { "$eq": ["$a", "$$v"] } }, "limit": 0 }],
                "let": vars,
            }
        };
        let (code, errmsg) = write_error(&dispatch_full(&with_let(doc! { "V": 1 }), c));
        assert_eq!(code, 9);
        assert!(
            errmsg.contains("'V' starts with an invalid character"),
            "{errmsg}"
        );
        let (code, _) = write_error(&dispatch_full(&with_let(doc! { "v": "$a" }), c));
        assert_eq!(code, 4890500);

        let reply = delete(c, doc! { "q": { "$expr": { "$nope": 1 } }, "limit": 0 });
        assert_eq!(
            write_error(&reply),
            (168, "Unrecognized expression '$nope'".to_string())
        );
        let reply = delete(c, doc! { "q": { "$text": { "$search": "x" } }, "limit": 0 });
        assert_eq!(
            write_error(&reply),
            (27, "text index required for $text query".to_string())
        );
        assert_eq!(stored(c).len(), 5);
    });
}

/// `count`'s `limit` is a whole number, a negative one counts as its size,
/// and `skip` is cut or rounded to one.
#[test]
fn count_reads_limit_and_skip_as_mongod_does() {
    with_wt(|c| {
        seed(c, rows());
        let count = |c: &mut CommandContext, extra: Document| {
            let mut command = doc! { "count": "c" };
            command.extend(extra);
            dispatch_full(&command, c)
        };
        let dec = |text: &str| Bson::Decimal128(text.parse().unwrap());
        for (extra, n) in [
            (doc! { "limit": -2 }, 2),
            (doc! { "limit": dec("2.0") }, 2),
            (doc! { "skip": 1.9 }, 4),
            (doc! { "skip": dec("1.5") }, 3),
            (doc! { "skip": dec("2.5") }, 3),
            (doc! { "skip": dec("0.5") }, 5),
            (doc! { "skip": f64::NAN }, 5),
            (doc! { "skip": f64::INFINITY }, 0),
        ] {
            assert_eq!(count(c, extra.clone()).get_i32("n"), Ok(n), "{extra}");
        }
        for (extra, code, errmsg) in [
            (
                doc! { "limit": dec("1.5") },
                9,
                "Cannot represent as a 64-bit integer: limit: 1.5",
            ),
            (
                doc! { "limit": f64::NAN },
                9,
                "Expected an integer, but found NaN in: limit: nan",
            ),
            (
                doc! { "limit": f64::INFINITY },
                9,
                "Cannot represent as a 64-bit integer: limit: inf",
            ),
            (doc! { "limit": 1.5 }, 9, "Expected an integer: limit: 1.5"),
            (
                doc! { "limit": i64::MIN },
                2,
                "limit value for count cannot be min long",
            ),
            (
                doc! { "skip": dec("-1.5") },
                2,
                "BSON field 'skip' value must be >= 0, actual value '-2'",
            ),
            (
                doc! { "let": {} },
                40415,
                "BSON field 'count.let' is an unknown field.",
            ),
            (
                doc! { "sampleId": 1 },
                14,
                "BSON field 'count.sampleId' is the wrong type 'int', expected type 'binData'",
            ),
            (
                doc! { "writeConcern": { "w": 1 } },
                72,
                "Command does not support writeConcern",
            ),
            (
                doc! { "readConcern": "local" },
                14,
                "BSON field 'count.readConcern' is the wrong type 'string', expected type 'object'",
            ),
            (
                doc! { "query": { "$text": { "$search": "x" } } },
                27,
                "text index required for $text query",
            ),
            (
                doc! { "query": { "$expr": { "$nope": 1 } } },
                168,
                "Unrecognized expression '$nope'",
            ),
        ] {
            assert_eq!(
                refusal(&count(c, extra.clone())),
                (code, errmsg.to_string()),
                "{extra}"
            );
        }
        // A `$text` count of a collection that is not there is 0.
        let reply = dispatch_full(
            &doc! { "count": "nope", "query": { "$text": { "$search": "x" } } },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(0), "{reply}");
        assert_eq!(
            refusal(&dispatch_full(&doc! { "count": ".a" }, c)),
            (73, "Collection names cannot start with '.': .a".to_string())
        );
    });
}

/// The collection names `distinct` will not read, and the options it checks.
#[test]
fn distinct_refuses_a_name_no_collection_can_have() {
    with_wt(|c| {
        seed(c, rows());
        for (command, code, errmsg) in [
            (
                doc! { "distinct": "a$b", "key": "a" },
                73,
                "Invalid collection name specified 't.a$b'",
            ),
            (
                doc! { "distinct": ".a", "key": "a" },
                73,
                "Collection names cannot start with '.': .a",
            ),
            (
                doc! { "distinct": "c", "key": "a", "writeConcern": { "w": 1 } },
                72,
                "Command does not support writeConcern",
            ),
            (
                doc! { "distinct": "c", "key": "a", "sampleId": 1 },
                14,
                "BSON field 'distinctCommandRequest.sampleId' is the wrong type 'int', expected type 'binData'",
            ),
            (
                doc! { "distinct": "nope", "key": "a", "query": { "$text": { "$search": "x" } } },
                27,
                "text index required for $text query (no such collection 't.nope')",
            ),
        ] {
            assert_eq!(
                refusal(&dispatch_full(&command, c)),
                (code, errmsg.to_string()),
                "{command}"
            );
        }
    });
}

/// A write concern's `j` and `fsync` are a bool or a number, and are not both
/// set; `wtimeout` is read whatever its type. An API version that is not "1"
/// is refused in mongod's words.
#[test]
fn a_write_concern_and_an_api_version_are_read_as_mongod_reads_them() {
    with_wt(|c| {
        seed(c, vec![]);
        let with = |c: &mut CommandContext, id: i32, concern: Document| {
            dispatch_full(
                &doc! { "insert": "c", "documents": [{ "_id": id }], "writeConcern": concern },
                c,
            )
        };
        assert_eq!(
            refusal(&with(c, 1, doc! { "w": 1, "j": true, "fsync": true })),
            (9, "fsync and j options cannot be used together".to_string())
        );
        let (code, errmsg) = refusal(&with(c, 1, doc! { "fsync": "y", "j": "x" }));
        assert_eq!(code, 14);
        assert!(
            errmsg.starts_with("BSON field 'WriteConcernOptions.fsync' is the wrong type 'string'"),
            "{errmsg}"
        );
        assert_eq!(applied(&with(c, 1, doc! { "wtimeout": "x" })), 1);
        assert_eq!(applied(&with(c, 2, doc! { "j": 2.5 })), 1);
        assert_eq!(applied(&with(c, 3, doc! { "j": false, "fsync": true })), 1);

        for (version, errmsg) in [
            ("2", "Cannot accept API version 2"),
            ("3", "API version must be \"1\""),
        ] {
            let reply = dispatch_full(&doc! { "ping": 1, "apiVersion": version }, c);
            assert_eq!(refusal(&reply), (322, errmsg.to_string()));
        }
    });
}

/// `$merge` with `whenMatched: "replace"` replaces the matched document in
/// place. It deleted the document and inserted the new one, so a replacement
/// a unique index refused left neither of them.
#[test]
fn a_merge_replacement_that_fails_keeps_the_document() {
    with_wt(|c| {
        let before = vec![
            doc! { "_id": 1, "z": 7 },
            doc! { "_id": 2, "z": 5, "k": 1 },
            doc! { "_id": 3, "z": 9 },
        ];
        seed(c, before.clone());
        index(c, doc! { "z": 1 }, "z_1", true);
        let reply = dispatch_full(
            &doc! {
                "aggregate": "c",
                "pipeline": [
                    { "$match": { "_id": 2 } },
                    { "$project": { "z": { "$literal": 7 } } },
                    { "$merge": { "into": "c", "whenMatched": "replace" } },
                ],
                "cursor": {},
            },
            c,
        );
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 11000);
        assert!(
            errmsg.contains("$merge failed due to a DuplicateKey error :: caused by :: Plan executor error during update :: caused by :: E11000"),
            "{errmsg}"
        );
        assert_eq!(reply.get_document("keyPattern"), Ok(&doc! { "z": 1 }));
        assert_eq!(stored(c), before);

        // One that succeeds keeps the document's place in the collection.
        let reply = dispatch_full(
            &doc! {
                "aggregate": "c",
                "pipeline": [
                    { "$match": { "_id": 2 } },
                    { "$project": { "b": { "$literal": 1 } } },
                    { "$merge": { "into": "c", "whenMatched": "replace" } },
                ],
                "cursor": {},
            },
            c,
        );
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        assert_eq!(
            stored(c),
            vec![
                doc! { "_id": 1, "z": 7 },
                doc! { "_id": 2, "b": 1 },
                doc! { "_id": 3, "z": 9 },
            ]
        );
    });
}
