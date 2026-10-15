//! The order a query returns documents in, and the document a write of one
//! picks, over real WiredTiger. Every expectation is what mongod 8.2.11
//! answered to the same command over the same documents
//! (`tools/probes/find_order.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

/// Twenty documents whose `a` repeats values, mixes types and holds arrays,
/// inserted in an order that is neither that of `a` nor of `_id`.
fn mixed() -> Vec<Document> {
    let values: Vec<(i32, Option<Bson>)> = vec![
        (11, Some(Bson::Int32(5))),
        (4, Some(Bson::Int32(3))),
        (17, Some(Bson::String("b".into()))),
        (2, Some(Bson::Int32(5))),
        (9, Some(Bson::Null)),
        (14, None),
        (6, Some(Bson::Double(3.0))),
        (20, Some(Bson::Array(vec![Bson::Int32(1), Bson::Int32(9)]))),
        (1, Some(Bson::String("a".into()))),
        (13, Some(Bson::Int32(7))),
        (8, Some(Bson::Boolean(true))),
        (16, Some(Bson::Document(doc! { "x": 1 }))),
        (3, Some(Bson::Int32(5))),
        (19, Some(Bson::Int64(3))),
        (7, Some(Bson::Array(vec![Bson::Int32(5), Bson::Int32(3)]))),
        (12, Some(Bson::Int32(1))),
        (
            5,
            Some(Bson::DateTime(bson::DateTime::from_millis(
                1_577_923_200_000,
            ))),
        ),
        (18, Some(Bson::String("B".into()))),
        (10, Some(Bson::Array(vec![]))),
        (15, Some(Bson::Double(9.5))),
    ];
    values
        .into_iter()
        .enumerate()
        .map(|(place, (id, a))| {
            let mut d = doc! { "_id": id };
            if let Some(a) = a {
                d.insert("a", a);
            }
            d.insert("b", (place % 4) as i32);
            d.insert("c", ((place * 7) % 5) as i32);
            d
        })
        .collect()
}

/// Ten documents with arrays of strings and arrays of sub-documents.
fn arrays() -> Vec<Document> {
    vec![
        doc! { "_id": 5, "t": ["x", "m"], "p": [{ "q": 3 }, { "q": 1 }], "n": 1 },
        doc! { "_id": 2, "t": ["a"], "p": [{ "q": 2 }], "n": 2 },
        doc! { "_id": 8, "t": "m", "p": { "q": 3 }, "n": 1 },
        doc! { "_id": 1, "t": ["m", "a", "z"], "p": [{ "q": 1 }, { "q": 5 }, { "r": 1 }], "n": 3 },
        doc! { "_id": 7, "t": [], "p": [], "n": 2 },
        doc! { "_id": 3, "t": ["z", "z"], "p": [{ "q": 5 }, { "q": 5 }], "n": 1 },
        doc! { "_id": 6, "n": 3 },
        doc! { "_id": 4, "t": [["m"], "b"], "p": [{ "q": [1, 3] }], "n": 2 },
        doc! { "_id": 9, "t": Bson::Null, "p": Bson::Null, "n": 1 },
        doc! { "_id": 10, "t": ["b", "a"], "p": [{ "q": 2 }, { "q": 3 }], "n": 3 },
    ]
}

fn seed(c: &mut CommandContext, docs: Vec<Document>, indexes: Vec<Document>) {
    dispatch_full(&doc! { "drop": "c" }, c);
    let reply = dispatch_full(&doc! { "insert": "c", "documents": docs }, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    assert!(!reply.contains_key("writeErrors"), "{reply}");
    if !indexes.is_empty() {
        let specs: Vec<Document> = indexes
            .into_iter()
            .map(|key| {
                let name: Vec<String> = key.iter().map(|(k, v)| format!("{k}_{v}")).collect();
                doc! { "key": key, "name": name.join("_") }
            })
            .collect();
        let reply = dispatch_full(&doc! { "createIndexes": "c", "indexes": specs }, c);
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
    }
}

fn ids_of(reply: &Document) -> Vec<i32> {
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

fn find(c: &mut CommandContext, filter: Document, rest: Document) -> Vec<i32> {
    let mut command = doc! { "find": "c", "filter": filter };
    command.extend(rest);
    ids_of(&dispatch_full(&command, c))
}

fn aggregate(c: &mut CommandContext, pipeline: Vec<Document>) -> Vec<i32> {
    ids_of(&dispatch_full(
        &doc! { "aggregate": "c", "pipeline": pipeline, "cursor": {} },
        c,
    ))
}

/// The `_id`s the collection holds, with the one a write marked.
fn hit(c: &mut CommandContext) -> Vec<i32> {
    let reply = dispatch_full(&doc! { "find": "c", "filter": { "hit": 1 } }, c);
    ids_of(&reply)
}

#[test]
fn a_query_reads_in_the_order_of_the_index_it_uses() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        // Without the index these come back as they were inserted.
        assert_eq!(
            find(c, doc! { "a": { "$ne": 5 } }, doc! {}),
            [10, 9, 14, 20, 12, 4, 6, 19, 13, 15, 18, 1, 17, 16, 8, 5]
        );
        assert_eq!(
            find(c, doc! { "a": { "$exists": true } }, doc! {}),
            [10, 9, 20, 12, 4, 6, 19, 7, 11, 2, 3, 13, 15, 18, 1, 17, 16, 8, 5]
        );
        assert_eq!(
            find(c, doc! { "a": { "$type": "string" } }, doc! {}),
            [18, 1, 17]
        );
        // The hinted `_id` index reads in `_id` order.
        assert_eq!(
            find(c, doc! { "a": 5 }, doc! { "hint": "_id_" }),
            [2, 3, 7, 11]
        );
    });
}

#[test]
fn two_bounds_over_an_array_are_met_by_different_elements() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        // `[1, 9]` holds an element over 1 and another under 9. A scan
        // between the two bounds reads neither of its keys.
        assert_eq!(
            find(c, doc! { "a": { "$gt": 1, "$lt": 9 } }, doc! {}),
            [20, 4, 6, 19, 7, 11, 2, 3, 13]
        );
        seed(c, mixed(), vec![doc! { "a": 1, "b": 1 }]);
        assert_eq!(
            find(c, doc! { "a": { "$gt": 1, "$lt": 9 } }, doc! {}),
            [20, 4, 19, 6, 7, 11, 3, 2, 13]
        );
    });
}

#[test]
fn a_backward_scan_meets_equal_keys_last_first() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "a": -1 } }),
            [5, 8, 16, 17, 1, 18, 15, 20, 13, 7, 3, 2, 11, 19, 6, 4, 12, 14, 9, 10]
        );
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "a": 1 } }),
            [10, 9, 14, 20, 12, 4, 6, 19, 7, 11, 2, 3, 13, 15, 18, 1, 17, 16, 8, 5]
        );
    });
}

#[test]
fn a_sort_on_an_array_field_with_bounds_is_a_sort_of_the_scan() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        // The scan of `[3, 3]` and `[5, 5]` runs forward, and the sort keeps
        // equal documents as the scan returned them.
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [5, 3] } },
                doc! { "sort": { "a": -1 } }
            ),
            [7, 11, 2, 3, 4, 6, 19]
        );
    });
}

#[test]
fn or_branches_on_one_index_are_one_scan() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        assert_eq!(
            find(
                c,
                doc! { "$or": [{ "a": { "$gt": 6 } }, { "a": { "$lt": 3 } }] },
                doc! {}
            ),
            [20, 12, 13, 15]
        );
        // Two indexes: the scans come in the order of the indexes, not of
        // the branches.
        seed(c, mixed(), vec![doc! { "a": 1 }, doc! { "b": 1 }]);
        assert_eq!(
            find(c, doc! { "$or": [{ "b": 1 }, { "a": 7 }] }, doc! {}),
            [13, 4, 14, 19, 18]
        );
    });
}

#[test]
fn the_index_of_the_sort_is_walked_when_that_finishes_first() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }, doc! { "b": 1 }]);
        // Nineteen of twenty match: walking `b_1` is done before a scan of
        // `a_1` has read its keys and sorted.
        assert_eq!(
            find(
                c,
                doc! { "a": { "$exists": true } },
                doc! { "sort": { "b": 1 } }
            ),
            [11, 9, 1, 3, 5, 4, 13, 19, 18, 17, 6, 8, 7, 10, 2, 20, 16, 12, 15]
        );
        // Four match: the scan of `a_1` and a sort is done first.
        assert_eq!(
            find(c, doc! { "a": 5 }, doc! { "sort": { "b": -1 } }),
            [2, 7, 11, 3]
        );
    });
}

#[test]
fn a_write_of_one_document_takes_the_first_in_that_order() {
    with_wt(|c| {
        // `{a: {$lt: 7}}` reads `a_1` from its low end: `[1, 9]` comes first,
        // where the first document inserted is `_id` 11.
        let q = doc! { "a": { "$lt": 7 } };
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{ "q": q.clone(), "u": { "$set": { "hit": 1 } } }] },
            c,
        );
        assert_eq!(reply.get_i32("nModified"), Ok(1), "{reply}");
        assert_eq!(hit(c), [20]);

        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "delete": "c", "deletes": [{ "q": q.clone(), "limit": 1 }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");
        assert!(!find(c, doc! {}, doc! {}).contains(&20));

        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": q.clone(), "update": { "$set": { "hit": 1 } } },
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id"),
            Ok(20),
            "{reply}"
        );

        // A hint names the index that is read.
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": q, "remove": true, "hint": "_id_" },
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id"),
            Ok(2),
            "{reply}"
        );
    });
}

#[test]
fn an_array_element_without_the_field_is_null() {
    with_wt(|c| {
        // `p: [{q: 1}, {q: 5}, {r: 1}]` has an element with no `q`.
        seed(c, arrays(), vec![doc! { "p.q": 1 }]);
        assert_eq!(find(c, doc! { "p.q": Bson::Null }, doc! {}), [1, 6, 9]);
        seed(c, arrays(), vec![]);
        assert_eq!(find(c, doc! { "p.q": Bson::Null }, doc! {}), [1, 6, 9]);
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "p.q": 1 } }),
            [1, 7, 6, 9, 5, 4, 2, 10, 8, 3]
        );
    });
}

#[test]
fn an_array_element_that_is_not_a_document_is_null_too() {
    with_wt(|c| {
        let mut docs = arrays();
        docs.push(doc! { "_id": 11, "p": [5, { "q": 4 }], "n": 1 });
        docs.push(doc! { "_id": 12, "p": [[{ "q": 0 }]], "n": 1 });
        seed(c, docs.clone(), vec![]);
        // It sorts as null, and `{"p.q": null}` still does not match it.
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "p.q": 1 } }),
            [1, 7, 6, 9, 11, 12, 5, 4, 2, 10, 8, 3]
        );
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "p.q": -1 } }),
            [1, 3, 11, 5, 8, 4, 10, 2, 7, 6, 9, 12]
        );
        assert_eq!(find(c, doc! { "p.q": Bson::Null }, doc! {}), [1, 6, 9]);
        seed(c, docs, vec![doc! { "p.q": 1 }]);
        assert_eq!(find(c, doc! { "p.q": Bson::Null }, doc! {}), [1, 6, 9]);
        assert_eq!(
            find(c, doc! {}, doc! { "hint": "p.q_1" }),
            [1, 7, 6, 9, 11, 12, 5, 4, 2, 10, 8, 3]
        );
    });
}

#[test]
fn a_write_that_brings_an_array_changes_how_the_index_is_read() {
    with_wt(|c| {
        seed(
            c,
            vec![
                doc! { "_id": 1, "a": 5 },
                doc! { "_id": 2, "a": 3 },
                doc! { "_id": 5, "a": 5 },
            ],
            vec![doc! { "a": 1 }],
        );
        let range = doc! { "a": { "$gt": 1, "$lt": 9 } };
        let from_three = doc! { "a": { "$gte": 3 } };
        let down = doc! { "sort": { "a": -1 } };
        assert_eq!(find(c, from_three.clone(), down.clone()), [5, 1, 2]);
        assert_eq!(find(c, range.clone(), doc! {}), [2, 1, 5]);
        // Two arrays arrive after the index has been read once.
        let reply = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 3, "a": [1, 9] }, { "_id": 4, "a": [5, 3] }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(2), "{reply}");
        assert_eq!(find(c, from_three.clone(), down), [3, 4, 5, 1, 2]);
        assert_eq!(find(c, range, doc! {}), [3, 2, 4, 1, 5]);
        assert_eq!(find(c, from_three, doc! {}), [2, 4, 1, 5, 3]);
    });
}

#[test]
fn an_upsert_that_matches_nothing_still_inserts() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{ "q": { "a": 99 }, "u": { "$set": { "hit": 1 } }, "upsert": true }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");
        assert_eq!(reply.get_array("upserted").map(Vec::len), Ok(1), "{reply}");
        // A `$natural: -1` hint reads from the last document: the one just
        // inserted, which already has the field.
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{
                "q": { "a": { "$gte": 3 } }, "u": { "$set": { "hit": 1 } }, "hint": { "$natural": -1 },
            }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");
        assert_eq!(reply.get_i32("nModified"), Ok(0), "{reply}");
    });
}

#[test]
fn a_collated_sort_is_not_read_from_an_index_of_plain_strings() {
    with_wt(|c| {
        let words = ["b", "A", "a", "B", "c", "á", "C", "ab", "Ab", "aB", ""];
        let mut docs: Vec<Document> = words
            .iter()
            .enumerate()
            .map(|(place, w)| doc! { "_id": place as i32, "w": *w })
            .collect();
        docs.push(doc! { "_id": 11, "w": Bson::Null });
        seed(c, docs, vec![doc! { "w": 1 }]);
        let ci = doc! { "locale": "en", "strength": 2 };
        // The collection is scanned and sorted: equal words as inserted.
        assert_eq!(
            find(
                c,
                doc! {},
                doc! { "sort": { "w": 1 }, "collation": ci.clone() }
            ),
            [11, 10, 1, 2, 5, 7, 8, 9, 0, 3, 4, 6]
        );
        // `$exists` reads the index, and the sort keeps equal words in its
        // order, which is that of the plain strings read backward.
        assert_eq!(
            find(
                c,
                doc! { "w": { "$exists": true } },
                doc! { "sort": { "w": -1 }, "collation": ci.clone() }
            ),
            [6, 4, 3, 0, 8, 9, 7, 5, 1, 2, 10, 11]
        );
        // A regex is still bounded by the index, whatever the collation.
        assert_eq!(
            find(
                c,
                doc! { "w": { "$regex": "^a" } },
                doc! { "collation": ci }
            ),
            [2, 9, 7]
        );
    });
}

#[test]
fn a_descending_index_reads_strings_in_reverse() {
    with_wt(|c| {
        let words = ["b", "A", "a", "B", "c", "á", "C", "ab", "Ab", "aB", ""];
        let mut docs: Vec<Document> = words
            .iter()
            .enumerate()
            .map(|(place, w)| doc! { "_id": place as i32, "w": *w })
            .collect();
        docs.push(doc! { "_id": 11, "w": Bson::Null });
        // `"a"` starts `"ab"`, and the empty string starts everything.
        seed(c, docs, vec![doc! { "w": 1 }]);
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "w": -1 } }),
            [5, 4, 0, 7, 9, 2, 6, 3, 8, 1, 10, 11]
        );
    });
}

#[test]
fn a_pipeline_sort_reads_in_the_order_of_the_plan() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        assert_eq!(
            aggregate(
                c,
                vec![doc! { "$match": {} }, doc! { "$sort": { "a": -1 } }]
            ),
            [5, 8, 16, 17, 1, 18, 15, 20, 13, 7, 3, 2, 11, 19, 6, 4, 12, 14, 9, 10]
        );
        // mongod moves the `$match` ahead of the `$sort`.
        assert_eq!(
            aggregate(
                c,
                vec![
                    doc! { "$sort": { "a": -1 } },
                    doc! { "$match": { "a": { "$lt": 7 } } }
                ]
            ),
            [20, 7, 11, 2, 3, 4, 6, 19, 12]
        );
    });
}

#[test]
fn a_merge_of_scans_hands_over_equal_documents_in_its_heap_order() {
    with_wt(|c| {
        // One scan per value of `a`, merged on `b` through a priority queue:
        // equal `b`s come out as libc++'s heap leaves them, not scan by scan.
        seed(c, mixed(), vec![doc! { "a": 1, "b": 1 }]);
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [5, 3] } },
                doc! { "sort": { "b": 1 } }
            ),
            [11, 3, 4, 19, 7, 6, 2]
        );
        // Read backward, the scans are merged greatest value first.
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [7, "b", 5, Bson::Null] } },
                doc! { "sort": { "b": -1 } }
            ),
            [2, 17, 7, 13, 14, 3, 9, 11]
        );
        seed(c, mixed(), vec![doc! { "a": -1, "b": 1 }]);
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [7, "b", 5, Bson::Null] } },
                doc! { "sort": { "b": 1 } }
            ),
            [11, 9, 3, 14, 13, 17, 7, 2]
        );
        seed(c, mixed(), vec![doc! { "a": 1, "b": -1 }]);
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [7, "b", 5, Bson::Null] } },
                doc! { "sort": { "b": 1 } }
            ),
            [3, 9, 11, 14, 13, 17, 7, 2]
        );
    });
}

#[test]
fn of_several_predicate_indexes_the_one_that_finishes_first_is_read() {
    with_wt(|c| {
        // `b_1` reads five keys and is done; `a_1` and `a_1_b_1` have two
        // documents each by then.
        seed(
            c,
            mixed(),
            vec![doc! { "a": 1 }, doc! { "b": 1 }, doc! { "a": 1, "b": 1 }],
        );
        assert_eq!(
            find(c, doc! { "a": { "$gt": 0 }, "b": 1 }, doc! {}),
            [4, 13, 19]
        );
    });
}

#[test]
fn a_sort_of_the_scan_is_kept_when_it_finishes_before_the_walk() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }]);
        let moved = vec![
            doc! { "update": "c", "updates": [{ "q": { "_id": 2 }, "u": { "$set": { "pad": "x".repeat(4000) } } }] },
            doc! { "delete": "c", "deletes": [{ "q": { "_id": { "$in": [11, 13] } }, "limit": 0 }] },
            doc! { "insert": "c", "documents": [{ "_id": 13, "a": 5, "b": 0, "c": 0 }, { "_id": 0, "a": 5, "b": 1 }] },
            doc! { "update": "c", "updates": [{ "q": { "_id": 4 }, "u": { "_id": 4, "a": 5, "b": 9 } }] },
            doc! { "update": "c", "updates": [{ "q": { "_id": 12 }, "u": { "$set": { "a": 5 } } }] },
            doc! { "update": "c", "updates": [{ "q": { "_id": 99 }, "u": { "$set": { "a": 5 } }, "upsert": true }] },
            doc! { "update": "c", "updates": [{ "q": { "b": 3 }, "u": { "$inc": { "c": 1 } }, "multi": true }] },
        ];
        for command in moved {
            let reply = dispatch_full(&command, c);
            assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        }
        // The scan of `[3, 3]` and `[5, 5]` ends on the key that follows
        // it, in the same work: with its sort it is done in 23 works, the
        // walk of `a_1` backward in 24.
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [5, 3] } },
                doc! { "projection": { "pad": 0 }, "sort": { "a": -1 } }
            ),
            [7, 4, 2, 3, 12, 13, 0, 99, 6, 19]
        );
    });
}

#[test]
fn a_match_behind_add_fields_is_read_through_its_index() {
    with_wt(|c| {
        seed(c, mixed(), vec![doc! { "a": 1 }, doc! { "b": 1 }]);
        // mongod moves the `$match` ahead of the stage, which writes nothing
        // it reads, and reads `b_1` for it.
        assert_eq!(
            aggregate(
                c,
                vec![
                    doc! { "$match": {} },
                    doc! { "$addFields": { "z": 1 } },
                    doc! { "$match": { "b": { "$gte": 1 } } }
                ]
            ),
            [4, 14, 13, 19, 18, 17, 6, 8, 7, 10, 2, 20, 16, 12, 15]
        );
        // A clause on the field the stage writes stays behind it.
        assert_eq!(
            aggregate(
                c,
                vec![
                    doc! { "$addFields": { "z": 1 } },
                    doc! { "$match": { "b": { "$gte": 2 }, "z": 1 } }
                ]
            ),
            [17, 6, 8, 7, 10, 2, 20, 16, 12, 15]
        );
    });
}

#[test]
fn a_regex_does_not_read_an_index_of_collation_keys() {
    with_wt(|c| {
        let words = ["b", "A", "a", "B", "c", "á", "C", "ab", "Ab", "aB", ""];
        let mut docs: Vec<Document> = words
            .iter()
            .enumerate()
            .map(|(place, w)| doc! { "_id": place as i32, "w": *w })
            .collect();
        docs.push(doc! { "_id": 11, "w": Bson::Null });
        seed(c, docs, vec![]);
        let ci = doc! { "locale": "en", "strength": 2 };
        let reply = dispatch_full(
            &doc! { "createIndexes": "c", "indexes": [
                { "key": { "w": 1 }, "name": "w_1" },
                { "key": { "w": 1 }, "name": "w_ci", "collation": ci.clone() },
            ] },
            c,
        );
        assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
        // `w_1` bounds the regex and sorts what it returns; the collated
        // index is only a walk for the sort, and finishes later.
        assert_eq!(
            find(
                c,
                doc! { "w": { "$regex": "^a" } },
                doc! { "sort": { "w": 1 }, "collation": ci }
            ),
            [2, 9, 7]
        );
    });
}

#[test]
fn a_merge_holds_the_documents_its_filter_rejects_too() {
    with_wt(|c| {
        // The FETCH that rejects `c` sits above the merge, so the rejected
        // documents take their places in its heap and move where equal
        // documents come out.
        seed(c, mixed(), vec![doc! { "a": 1, "b": 1 }]);
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [5, 3] }, "c": { "$ne": 1 } },
                doc! { "sort": { "b": 1 } }
            ),
            [11, 3, 4, 7, 6]
        );
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [5, 3, 1, 7, "a"] }, "c": { "$ne": 0 } },
                doc! { "sort": { "b": 1 } }
            ),
            [1, 3, 13, 4, 19, 7, 6, 20, 2]
        );
        assert_eq!(
            find(
                c,
                doc! { "a": { "$in": [7, "b", 5, Bson::Null] }, "c": { "$ne": 4 } },
                doc! { "sort": { "b": -1 } }
            ),
            [2, 7, 13, 14, 9, 11]
        );
    });
}

#[test]
fn a_write_over_one_plain_index_takes_the_first_in_its_order() {
    with_wt(|c| {
        // One plain index on the predicate's field: the store reads it as
        // mongod's plan does, by key and then insertion, so the write takes
        // the first `3` without the matches being read twice.
        let docs = || {
            vec![
                doc! { "_id": 1, "a": 5 },
                doc! { "_id": 2, "a": 3 },
                doc! { "_id": 3, "a": 9 },
                doc! { "_id": 4, "a": 3 },
                doc! { "_id": 5, "a": 1 },
                doc! { "_id": 6, "a": 3.0 },
            ]
        };
        seed(c, docs(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "update": "c", "updates": [{ "q": { "a": { "$gt": 2 } }, "u": { "$set": { "hit": 1 } } }] },
            c,
        );
        assert_eq!(reply.get_i32("nModified"), Ok(1), "{reply}");
        assert_eq!(hit(c), [2]);
        seed(c, docs(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "delete": "c", "deletes": [{ "q": { "a": { "$lt": 9, "$gte": 3 } }, "limit": 1 }] },
            c,
        );
        assert_eq!(reply.get_i32("n"), Ok(1), "{reply}");
        assert_eq!(
            find(c, doc! {}, doc! { "sort": { "_id": 1 } }),
            [1, 3, 4, 5, 6]
        );
        seed(c, docs(), vec![doc! { "a": 1 }]);
        let reply = dispatch_full(
            &doc! { "findAndModify": "c", "query": { "a": 3 }, "update": { "$set": { "hit": 1 } } },
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id"),
            Ok(2),
            "{reply}"
        );
    });
}
