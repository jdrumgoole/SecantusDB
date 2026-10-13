//! `$out` and `$merge` over real WiredTiger: what they leave in the target
//! and what they refuse. Every expectation is what mongod 8.2.11 answered to
//! the same pipeline (`tools/probes/out_and_merge.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn seed(c: &mut CommandContext, coll: &str, docs: Vec<Document>) {
    dispatch_full(&doc! { "drop": coll }, c);
    if !docs.is_empty() {
        let reply = dispatch_full(&doc! { "insert": coll, "documents": docs }, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        assert!(!reply.contains_key("writeErrors"), "{reply}");
    }
}

fn unique(c: &mut CommandContext, key: Document) {
    let reply = dispatch_full(
        &doc! { "createIndexes": "t", "indexes": [{ "key": key, "name": "u", "unique": true }] },
        c,
    );
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
}

/// The documents a collection holds, in the order it holds them.
fn stored(c: &mut CommandContext, coll: &str) -> Vec<Document> {
    let reply = dispatch_full(&doc! { "find": coll }, c);
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn aggregate(c: &mut CommandContext, pipeline: Vec<Document>) -> Document {
    dispatch_full(
        &doc! { "aggregate": "c", "pipeline": pipeline, "cursor": {} },
        c,
    )
}

fn merge(c: &mut CommandContext, spec: impl Into<Bson>) -> Document {
    aggregate(c, vec![doc! { "$merge": spec.into() }])
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

const DURING: &str = "Executor error during aggregate command on namespace: t.c :: caused by :: ";

fn source() -> Vec<Document> {
    vec![
        doc! { "_id": 1, "a": 10, "k": "x", "o": { "p": 1 } },
        doc! { "_id": 2, "a": 20, "k": "y", "o": { "q": 2 } },
        doc! { "_id": 3, "a": 30, "k": "z" },
    ]
}

fn target() -> Vec<Document> {
    vec![
        doc! { "_id": 2, "b": 2, "k": "y", "o": { "p": 7, "r": 8 } },
        doc! { "_id": 9, "b": 9, "k": "w" },
        doc! { "_id": 1, "b": 1, "k": "x", "a": 0 },
    ]
}

/// `whenMatched: "merge"` is a `$set` of the document: a top-level field is
/// set whole, a name with a dot is a path, and new fields land in `$set`'s
/// order. It merged sub-documents field by field.
#[test]
fn merge_is_a_set_of_the_document() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", target());
        ok(&merge(c, "t"));
        assert_eq!(
            stored(c, "t"),
            vec![
                doc! { "_id": 2, "b": 2, "k": "y", "o": { "q": 2 }, "a": 20 },
                doc! { "_id": 9, "b": 9, "k": "w" },
                doc! { "_id": 1, "b": 1, "k": "x", "a": 10, "o": { "p": 1 } },
                doc! { "_id": 3, "a": 30, "k": "z" },
            ]
        );

        let unsorted = doc! { "_id": 9, "z": 1, "b": 2, "m": { "y": 1, "a": 2 }, "a.b": 5 };
        seed(c, "c", vec![unsorted.clone()]);
        seed(c, "t", vec![]);
        ok(&merge(c, "t"));
        assert_eq!(
            stored(c, "t"),
            vec![doc! { "_id": 9, "a": { "b": 5 }, "b": 2, "m": { "y": 1, "a": 2 }, "z": 1 }]
        );
        // `replace`, `fail` and a pipeline store the document as it came.
        for matched in [
            Bson::from("replace"),
            Bson::from("fail"),
            Bson::Array(vec![]),
        ] {
            seed(c, "t", vec![]);
            ok(&merge(
                c,
                doc! { "into": "t", "whenMatched": matched.clone() },
            ));
            assert_eq!(stored(c, "t"), vec![unsorted.clone()], "{matched}");
        }
    });
}

/// What each pair of modes leaves, and the pairs mongod does not have.
#[test]
fn every_pair_of_modes() {
    with_wt(|c| {
        let run = |c: &mut CommandContext, matched: &str, absent: &str| {
            seed(c, "c", source());
            seed(c, "t", target());
            let reply = merge(
                c,
                doc! { "into": "t", "whenMatched": matched, "whenNotMatched": absent },
            );
            let ids: Vec<i32> = stored(c, "t")
                .iter()
                .map(|d| d.get_i32("_id").unwrap())
                .collect();
            (reply, ids)
        };
        let (reply, ids) = run(c, "replace", "insert");
        ok(&reply);
        assert_eq!(ids, vec![2, 9, 1, 3]);
        assert_eq!(
            stored(c, "t")[0],
            doc! { "_id": 2, "a": 20, "k": "y", "o": { "q": 2 } }
        );
        let (reply, ids) = run(c, "merge", "discard");
        ok(&reply);
        assert_eq!(ids, vec![2, 9, 1]);
        let (reply, _) = run(c, "keepExisting", "insert");
        ok(&reply);
        assert_eq!(stored(c, "t")[0], target()[0]);

        // The matched documents are written before the one with no match is
        // reported.
        let (reply, ids) = run(c, "merge", "fail");
        assert_eq!(
            refusal(&reply),
            (
                13113,
                format!(
                    "{DURING}$merge could not find a matching document in the target collection \
                     for at least one document in the source collection"
                )
            )
        );
        assert_eq!(ids, vec![2, 9, 1]);
        assert_eq!(stored(c, "t")[2].get_i32("a"), Ok(10));

        // `fail` inserts what is not there before it reports what is.
        let (reply, ids) = run(c, "fail", "insert");
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 11000);
        assert!(
            errmsg.starts_with(&format!(
                "{DURING}$merge with whenMatched: fail found an existing document with the same \
                 values for the 'on' fields :: caused by :: E11000"
            )),
            "{errmsg}"
        );
        assert_eq!(reply.get_document("keyPattern"), Ok(&doc! { "_id": 1 }));
        assert_eq!(ids, vec![2, 9, 1, 3]);

        for (matched, absent) in [("keepExisting", "discard"), ("fail", "fail")] {
            let (reply, ids) = run(c, matched, absent);
            assert_eq!(
                refusal(&reply),
                (
                    51181,
                    format!(
                        "Combination of $merge modes 'whenMatched: {matched}' and \
                         'whenNotMatched: {absent}' is not supported"
                    )
                )
            );
            assert_eq!(ids, vec![2, 9, 1]);
        }
    });
}

/// A `whenMatched` pipeline sees the incoming document as `$$new` and the
/// stage's `let`, and a path into either that leads nowhere removes the
/// field it is set to.
#[test]
fn a_when_matched_pipeline_and_its_variables() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", target());
        ok(&merge(
            c,
            doc! {
                "into": "t",
                "let": { "v": "$a" },
                "whenMatched": [{ "$set": { "m": { "$add": ["$b", "$$v"] }, "n": "$$new.k" } }],
            },
        ));
        let after = stored(c, "t");
        assert_eq!(after[0].get_i32("m"), Ok(22));
        assert_eq!(after[0].get_str("n"), Ok("y"));
        assert_eq!(after[3], source()[2]);

        // `$$new.nope` is missing, and a field set to it goes.
        seed(c, "t", target());
        ok(&merge(
            c,
            doc! { "into": "t", "whenMatched": [{ "$set": { "b": "$$new.nope" } }] },
        ));
        assert!(!stored(c, "t")[0].contains_key("b"));

        for (spec, code, errmsg) in [
            (
                doc! { "into": "t", "let": { "v": 5 } },
                51199,
                "Cannot use 'let' variables with 'whenMatched: merge' mode".to_string(),
            ),
            (
                doc! { "into": "t", "let": { "new": "$a" }, "whenMatched": [] },
                51273,
                "'let' may not define a value for the reserved 'new' variable other than \
                 '$$ROOT'"
                    .to_string(),
            ),
            (
                doc! { "into": "t", "let": { "v": "$$nope" }, "whenMatched": [] },
                17276,
                "Use of undefined variable: nope".to_string(),
            ),
            (
                doc! { "into": "t", "whenMatched": [{ "$nope": 1 }] },
                40324,
                "Unrecognized pipeline stage name: '$nope'".to_string(),
            ),
            (
                doc! { "into": "t", "whenMatched": [{ "$match": {} }] },
                72,
                format!("{DURING}$match is not allowed to be used within an update"),
            ),
        ] {
            seed(c, "t", target());
            assert_eq!(refusal(&merge(c, spec.clone())), (code, errmsg), "{spec}");
            assert_eq!(stored(c, "t"), target(), "{spec}");
        }
    });
}

/// The `on` fields need a unique index that holds every document and
/// compares as the pipeline does, and a document is matched by their values.
#[test]
fn on_fields_and_their_index() {
    with_wt(|c| {
        let missing = (
            51183,
            "Cannot find index to verify that join fields will be unique".to_string(),
        );
        seed(c, "c", source());
        seed(c, "t", target());
        assert_eq!(refusal(&merge(c, doc! { "into": "t", "on": "k" })), missing);
        dispatch_full(
            &doc! { "createIndexes": "t", "indexes": [{
                "key": { "k": 1 }, "name": "p", "unique": true,
                "partialFilterExpression": { "b": { "$gt": 0 } },
            }] },
            c,
        );
        assert_eq!(refusal(&merge(c, doc! { "into": "t", "on": "k" })), missing);

        seed(c, "t", target());
        unique(c, doc! { "k": 1 });
        ok(&merge(c, doc! { "into": "t", "on": "k" }));
        let after = stored(c, "t");
        assert_eq!(
            after[0],
            doc! { "_id": 2, "b": 2, "k": "y", "o": { "q": 2 }, "a": 20 }
        );
        // The unmatched document is the upsert of `{k: "z"}`.
        assert_eq!(after[3], doc! { "_id": 3, "k": "z", "a": 30 });

        // A matched document with another `_id` cannot take the new one.
        seed(c, "c", vec![doc! { "_id": 5, "k": "x", "q": 1 }]);
        let (code, errmsg) = refusal(&merge(c, doc! { "into": "t", "on": "k" }));
        assert_eq!(code, 66);
        assert!(
            errmsg.starts_with(&format!(
                "{DURING}$merge failed to update the matching document, did you attempt to \
                 modify the _id or the shard key? :: caused by :: Plan executor error during \
                 update :: caused by :: Performing an update on the path '_id'"
            )),
            "{errmsg}"
        );
        seed(c, "c", vec![doc! { "_id": 5, "k": [1, 2] }]);
        assert_eq!(
            refusal(&merge(c, doc! { "into": "t", "on": "k" })),
            (
                51185,
                format!("{DURING}$merge write error: 'on' field 'k' is an array")
            )
        );

        for (on, code, errmsg) in [
            (
                Bson::Int32(1),
                51186,
                "$merge 'on' field  must be either a string or an array of strings, but found int",
            ),
            (
                Bson::Array(vec![]),
                51187,
                "If explicitly specifying $merge 'on', must include at least one field",
            ),
            (
                Bson::Array(vec!["k".into(), "k".into()]),
                31465,
                "Found a duplicate field 'k'",
            ),
        ] {
            assert_eq!(
                refusal(&merge(c, doc! { "into": "t", "on": on })),
                (code, errmsg.to_string())
            );
        }
    });
}

/// A write the target refuses does not stop the others, and the validator
/// is asked about each document as it is written.
#[test]
fn a_refused_write_leaves_the_others_written() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", vec![]);
        ok(&dispatch_full(
            &doc! { "create": "t", "validator": { "a": { "$gt": 15 } } },
            c,
        ));
        for matched in [Bson::from("merge"), Bson::Array(vec![]), Bson::from("fail")] {
            dispatch_full(
                &doc! { "delete": "t", "deletes": [{ "q": {}, "limit": 0 }] },
                c,
            );
            let reply = merge(c, doc! { "into": "t", "whenMatched": matched.clone() });
            let (code, errmsg) = refusal(&reply);
            assert_eq!(code, 121, "{matched}");
            assert!(errmsg.ends_with("Document failed validation"), "{errmsg}");
            assert!(reply.contains_key("errInfo"), "{reply}");
            let ids: Vec<i32> = stored(c, "t")
                .iter()
                .map(|d| d.get_i32("_id").unwrap())
                .collect();
            assert_eq!(ids, vec![2, 3], "{matched}");
        }

        seed(
            c,
            "t",
            vec![doc! { "_id": 1, "a": 5 }, doc! { "_id": 7, "a": 10 }],
        );
        unique(c, doc! { "a": 1 });
        let reply = merge(c, doc! { "into": "t", "whenMatched": "replace" });
        let (code, errmsg) = refusal(&reply);
        assert_eq!(code, 11000);
        assert!(
            errmsg.starts_with(&format!(
                "{DURING}$merge failed due to a DuplicateKey error :: caused by :: Plan executor \
                 error during update :: caused by :: E11000"
            )),
            "{errmsg}"
        );
        assert_eq!(reply.get_document("keyValue"), Ok(&doc! { "a": 10 }));
        assert_eq!(stored(c, "t").len(), 4);
        assert_eq!(stored(c, "t")[0], doc! { "_id": 1, "a": 5 });
    });
}

/// What the stage's own fields are refused for, in the order mongod reads
/// them, and the targets it will not write.
#[test]
fn the_merge_spec_is_read_as_mongod_reads_it() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", target());
        let cases: Vec<(Bson, i32, String)> = vec![
            (
                doc! { "into": "" }.into(),
                5786800,
                "$merge 'into' field cannot be an empty string".into(),
            ),
            (
                doc! { "into": 1 }.into(),
                51178,
                "$merge 'into' field  must be either a string or an object, but found int".into(),
            ),
            (
                doc! { "into": { "db": "x" } }.into(),
                5786801,
                "$merge 'into' field must specify a 'coll' that is not empty, null or undefined"
                    .into(),
            ),
            (
                doc! { "into": { "coll": "t", "nope": 1 } }.into(),
                40415,
                "BSON field 'into.nope' is an unknown field.".into(),
            ),
            (
                doc! { "into": "t", "whenMatched": "delete" }.into(),
                2,
                "Enumeration value 'delete' for field 'whenMatched' is not a valid value.".into(),
            ),
            (
                doc! { "into": "t", "whenMatched": 1 }.into(),
                51191,
                "$merge 'whenMatched' field  must be either a string or an array, but found int"
                    .into(),
            ),
            (
                doc! { "into": "t", "whenNotMatched": "nope", "whenMatched": "nope" }.into(),
                2,
                "Enumeration value 'nope' for field '$merge.whenNotMatched' is not a valid value."
                    .into(),
            ),
            (
                doc! { "into": "t", "on": 1, "nope": 1 }.into(),
                51186,
                "$merge 'on' field  must be either a string or an array of strings, but found int"
                    .into(),
            ),
            (
                doc! { "into": "system.x" }.into(),
                31319,
                "Cannot $merge to special collection: system.x".into(),
            ),
            (
                doc! { "into": "a$b" }.into(),
                73,
                format!("{DURING}Invalid collection name: a$b"),
            ),
        ];
        for (spec, code, errmsg) in cases {
            assert_eq!(refusal(&merge(c, spec.clone())), (code, errmsg), "{spec}");
        }
        assert_eq!(stored(c, "t"), target());
        // An empty database name is this one; nothing is created for a
        // pipeline that writes nothing.
        ok(&merge(c, doc! { "into": { "db": "", "coll": "t" } }));
        assert_eq!(stored(c, "t").len(), 4);
        ok(&aggregate(
            c,
            vec![doc! { "$match": { "_id": 99 } }, doc! { "$merge": "fresh" }],
        ));
        let listed = dispatch_full(
            &doc! { "listCollections": 1, "filter": { "name": "fresh" } },
            c,
        );
        let found = listed
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert!(found.is_empty(), "{listed}");
    });
}

/// A writing stage runs once, at the end of the command's own pipeline.
#[test]
fn a_writing_stage_inside_another_stage_is_refused() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", target());
        for stage in ["$merge", "$out"] {
            let write = doc! { stage: "t" };
            for (outer, code, errmsg) in [
                (
                    doc! { "$facet": { "x": [write.clone()] } },
                    40600,
                    format!("{stage} is not allowed to be used within a $facet stage"),
                ),
                (
                    doc! { "$lookup": { "from": "t", "pipeline": [write.clone()], "as": "x" } },
                    51047,
                    format!("{stage} is not allowed within a $lookup's sub-pipeline"),
                ),
                (
                    doc! { "$unionWith": { "coll": "t", "pipeline": [write.clone()] } },
                    31441,
                    format!("{stage} is not allowed within a $unionWith's sub-pipeline"),
                ),
            ] {
                assert_eq!(refusal(&aggregate(c, vec![outer])), (code, errmsg));
                assert_eq!(stored(c, "t"), target());
            }
        }
    });
}

/// The targets `$out` will not write, and the documents it will not store.
#[test]
fn out_refuses_a_target_or_a_document_and_leaves_the_target() {
    with_wt(|c| {
        seed(c, "c", source());
        seed(c, "t", target());
        let out = |c: &mut CommandContext, before: Option<Document>, spec: Bson| {
            let mut pipeline: Vec<Document> = before.into_iter().collect();
            pipeline.push(doc! { "$out": spec });
            aggregate(c, pipeline)
        };
        let cases: Vec<(Option<Document>, Bson, i32, String)> = vec![
            (
                None,
                doc! { "coll": "t" }.into(),
                40414,
                "BSON field '$out.db' is missing but a required field".into(),
            ),
            (
                None,
                doc! { "db": "", "coll": "t" }.into(),
                73,
                "Invalid $out target namespace, .t".into(),
            ),
            (
                None,
                doc! { "db": 1, "coll": "t" }.into(),
                14,
                "BSON field '$out.db' is the wrong type 'int', expected type 'string'".into(),
            ),
            (
                None,
                "system.x".into(),
                17385,
                "Can't $out to special collection: system.x".into(),
            ),
            (
                None,
                "a$b".into(),
                20,
                format!("{DURING}error with target namespace: Invalid collection name: a$b"),
            ),
            (
                None,
                doc! { "db": "t", "coll": "t", "timeseries": { "timeField": "ts" } }.into(),
                7268700,
                format!(
                    "{DURING}Cannot create a time-series collection from a non time-series \
                     collection or view."
                ),
            ),
            (
                Some(doc! { "$set": { "_id": [1] } }),
                "t".into(),
                53,
                format!("{DURING}The '_id' value cannot be of type array"),
            ),
            (
                Some(doc! { "$set": { "_id": { "$literal": { "$x": 1 } } } }),
                "t".into(),
                52,
                format!(
                    "{DURING}_id fields may not contain '$'-prefixed fields: $x is not valid for \
                     storage."
                ),
            ),
        ];
        for (before, spec, code, errmsg) in cases {
            let reply = out(c, before, spec.clone());
            assert_eq!(refusal(&reply), (code, errmsg), "{spec}");
            assert_eq!(stored(c, "t"), target(), "{spec}");
        }
        // A unique index the output breaks is reported with its key.
        unique(c, doc! { "k": 1 });
        let reply = out(c, Some(doc! { "$set": { "k": "same" } }), "t".into());
        assert_eq!(reply.get_i32("code"), Ok(11000), "{reply}");
        assert_eq!(reply.get_document("keyValue"), Ok(&doc! { "k": "same" }));
        assert_eq!(stored(c, "t"), target());
    });
}

/// A replacement may not turn the `_id` `"a"` into `"A"`, whatever
/// collation matched them; and a write that leaves a `$`-named field as it
/// was is no write of it.
#[test]
fn an_id_is_not_changed_by_a_collation_and_a_dollar_field_may_stay() {
    with_wt(|c| {
        dispatch_full(&doc! { "drop": "t" }, c);
        ok(&dispatch_full(
            &doc! { "create": "t", "collation": { "locale": "en", "strength": 2 } },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "insert": "t", "documents": [{ "_id": "a", "y": 1 }] },
            c,
        ));
        seed(c, "c", vec![doc! { "_id": "A", "a": 1 }]);
        let (code, errmsg) = refusal(&merge(c, doc! { "into": "t", "whenMatched": "replace" }));
        assert_eq!(code, 66);
        assert!(
            errmsg.ends_with("found to have been altered to _id: \"A\""),
            "{errmsg}"
        );
        assert_eq!(stored(c, "t"), vec![doc! { "_id": "a", "y": 1 }]);

        // Three documents that become one: the first is an upsert and the
        // other two set `$c` to what it already is.
        seed(c, "c", source());
        seed(c, "t", vec![]);
        ok(&aggregate(
            c,
            vec![
                doc! { "$replaceWith": { "$literal": { "_id": 1, "a.b": 1, "$c": 2 } } },
                doc! { "$merge": "t" },
            ],
        ));
        assert_eq!(
            stored(c, "t"),
            vec![doc! { "_id": 1, "$c": 2, "a": { "b": 1 } }]
        );
        let reply = dispatch_full(
            &doc! { "update": "t", "updates": [{ "q": { "_id": 1 }, "u": { "$set": { "$c": 3 } } }] },
            c,
        );
        let errors = reply.get_array("writeErrors").unwrap();
        assert_eq!(errors[0].as_document().unwrap().get_i32("code"), Ok(52));
    });
}
