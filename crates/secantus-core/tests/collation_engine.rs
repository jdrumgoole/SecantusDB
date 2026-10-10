//! What a collation changes in the aggregation, query and update engines, and
//! a few rules found beside it. Every expectation was measured against mongod
//! 8.2.11 on 2026-10-10 (`tools/probes/collation.py`).

use bson::{doc, Bson, Document};
use secantus_core::collation::{self, Collation, Context};
use secantus_core::{aggregate, query, update};

fn strength(n: i32) -> Collation {
    collation::parse_strict(&doc! {"locale": "en", "strength": n}, Context::COMMAND)
        .unwrap()
        .unwrap()
}

fn run(docs: Vec<Document>, pipeline: Vec<Document>, coll: Option<&Collation>) -> Vec<Document> {
    let stages: Vec<Bson> = pipeline.into_iter().map(Bson::Document).collect();
    aggregate::apply_pipeline(docs, &stages, &Document::new(), coll).expect("pipeline")
}

fn eval(expr: Bson, coll: Option<&Collation>) -> Bson {
    let out = run(vec![doc! {}], vec![doc! {"$project": {"r": expr}}], coll);
    out[0].get("r").cloned().unwrap()
}

fn letters() -> Vec<Document> {
    vec![
        doc! {"_id": 0, "s": "b"},
        doc! {"_id": 1, "s": "B"},
        doc! {"_id": 2, "s": "z"},
    ]
}

#[test]
fn expressions_compare_by_the_collation() {
    let s2 = strength(2);
    let on = Some(&s2);
    assert_eq!(
        eval(bson::bson!({"$eq": ["a", "A"]}), on),
        Bson::Boolean(true)
    );
    assert_eq!(
        eval(bson::bson!({"$eq": ["a", "A"]}), None),
        Bson::Boolean(false)
    );
    assert_eq!(eval(bson::bson!({"$cmp": ["a", "B"]}), on), Bson::Int32(-1));
    assert_eq!(
        eval(bson::bson!({"$cmp": ["a", "B"]}), None),
        Bson::Int32(1)
    );
    assert_eq!(
        eval(bson::bson!({"$in": ["A", ["a", "b"]]}), on),
        Bson::Boolean(true)
    );
    assert_eq!(
        eval(bson::bson!({"$setEquals": [["a", "b"], ["A", "B"]]}), on),
        Bson::Boolean(true)
    );
    assert_eq!(
        eval(bson::bson!({"$eq": [{"k": ["a"]}, {"k": ["A"]}]}), on),
        Bson::Boolean(true)
    );
    // Not everything follows it: these read the characters themselves.
    assert_eq!(
        eval(bson::bson!({"$indexOfCP": ["xAx", "a"]}), on),
        Bson::Int32(-1)
    );
    assert_eq!(
        eval(
            bson::bson!({"$regexMatch": {"input": "A", "regex": "a"}}),
            on
        ),
        Bson::Boolean(false)
    );
    // And nothing leaks out of the pipeline that set it.
    assert!(!collation::is_active());
}

#[test]
fn group_keys_and_sorts_follow_the_collation() {
    let s2 = strength(2);
    let grouped = run(
        letters(),
        vec![
            doc! {"$group": {"_id": "$s", "n": {"$sum": 1}}},
            doc! {"$sort": {"n": -1}},
        ],
        Some(&s2),
    );
    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].get_i32("n").ok(), Some(2));
    assert_eq!(
        run(letters(), vec![doc! {"$group": {"_id": "$s"}}], None).len(),
        3
    );

    let sorted = run(
        vec![
            doc! {"_id": 0, "s": "b"},
            doc! {"_id": 1, "s": "A"},
            doc! {"_id": 2, "s": "a"},
        ],
        vec![doc! {"$sort": {"s": 1, "_id": 1}}],
        Some(&s2),
    );
    let order: Vec<i32> = sorted.iter().map(|d| d.get_i32("_id").unwrap()).collect();
    assert_eq!(order, vec![1, 2, 0]);
}

#[test]
fn a_tie_in_min_and_max_goes_to_the_later_value() {
    // Equal is not identical: int 1, double 1, long 1.
    let docs = || {
        vec![
            doc! {"_id": 0, "x": 1i32},
            doc! {"_id": 1, "x": 1.0f64},
            doc! {"_id": 2, "x": 1i64},
        ]
    };
    let group = doc! {"$group": {"_id": null, "lo": {"$min": "$x"}, "hi": {"$max": "$x"}}};
    let out = run(docs(), vec![group.clone()], None);
    assert_eq!(out[0].get("lo"), Some(&Bson::Int64(1)));
    assert_eq!(out[0].get("hi"), Some(&Bson::Int64(1)));
    let mut reversed = docs();
    reversed.reverse();
    let out = run(reversed, vec![group], None);
    assert_eq!(out[0].get("lo"), Some(&Bson::Int32(1)));

    let s2 = strength(2);
    let out = run(
        letters(),
        vec![doc! {"$group": {"_id": null, "lo": {"$min": "$s"}}}],
        Some(&s2),
    );
    assert_eq!(out[0].get_str("lo").ok(), Some("B"));
}

#[test]
fn max_n_lists_the_later_of_equal_values_first() {
    let input = bson::bson!([1i32, 2.0f64, 2i64, 0i32, 2i32]);
    assert_eq!(
        eval(
            bson::bson!({"$maxN": {"input": input.clone(), "n": 2}}),
            None
        ),
        bson::bson!([2i64, 2.0f64])
    );
    assert_eq!(
        eval(
            bson::bson!({"$minN": {"input": [1i32, 1.0f64, 1i64], "n": 2}}),
            None
        ),
        bson::bson!([1i32, 1.0f64])
    );
    let s2 = strength(2);
    assert_eq!(
        eval(
            bson::bson!({"$maxN": {"input": ["b", "A", "a", "B"], "n": 2}}),
            Some(&s2)
        ),
        bson::bson!(["B", "b"])
    );
}

#[test]
fn expr_in_against_constants_never_widens_under_a_collation() {
    let s2 = strength(2);
    let matched = |filter: Document| -> Vec<i32> {
        letters()
            .iter()
            .filter(|d| query::matches(d, &filter, &Document::new(), Some(&s2)).unwrap())
            .map(|d| d.get_i32("_id").unwrap())
            .collect()
    };
    // mongod's own behaviour, reproduced: see `query::expr_in_by_value`.
    assert_eq!(
        matched(doc! {"$expr": {"$in": ["$s", ["B", "Z"]]}}),
        vec![1]
    );
    assert_eq!(matched(doc! {"$expr": {"$eq": ["$s", "B"]}}), vec![0, 1]);
    assert_eq!(
        matched(doc! {"$expr": {"$or": [{"$in": ["$s", ["B", "Z"]]}]}}),
        vec![0, 1, 2]
    );
    assert_eq!(matched(doc! {"s": {"$in": ["B", "Z"]}}), vec![0, 1, 2]);
    // A schema is by value alone.
    assert_eq!(
        matched(doc! {"$jsonSchema": {"properties": {"s": {"enum": ["B"]}}}}),
        vec![1]
    );
}

#[test]
fn always_true_and_always_false() {
    let d = doc! {"a": 1};
    let m = |f: Document| query::matches(&d, &f, &Document::new(), None);
    assert_eq!(m(doc! {"$alwaysTrue": 1}).ok(), Some(true));
    assert_eq!(m(doc! {"$alwaysFalse": 1}).ok(), Some(false));
    assert!(m(doc! {"$alwaysTrue": 0}).is_err());
}

#[test]
fn a_sort_reads_an_array_by_its_smallest_or_largest_element() {
    let docs = vec![
        doc! {"_id": 0, "g": "a"},
        doc! {"_id": 1, "g": "A"},
        doc! {"_id": 2, "g": ["a", "B"]},
    ];
    let ranked = run(
        docs.clone(),
        vec![
            doc! {"$setWindowFields": {"sortBy": {"g": 1}, "output": {"r": {"$rank": {}}}}},
            doc! {"$sort": {"_id": 1}},
        ],
        None,
    );
    let ranks: Vec<i32> = ranked.iter().map(|d| d.get_i32("r").unwrap()).collect();
    assert_eq!(ranks, vec![3, 1, 2]);
    let top = run(
        docs,
        vec![doc! {"$group": {"_id": null,
        "r": {"$top": {"sortBy": {"g": -1, "_id": 1}, "output": "$_id"}}}}],
        None,
    );
    assert_eq!(top[0].get_i32("r").ok(), Some(0));
}

#[test]
fn fill_writes_null_and_emits_partitions_in_key_order() {
    let out = run(
        letters(),
        vec![
            doc! {"$fill": {"partitionByFields": ["s"], "sortBy": {"_id": 1},
            "output": {"q": {"method": "locf"}}}},
        ],
        None,
    );
    let order: Vec<i32> = out.iter().map(|d| d.get_i32("_id").unwrap()).collect();
    assert_eq!(order, vec![1, 0, 2]);
    assert!(out.iter().all(|d| d.get("q") == Some(&Bson::Null)));
}

#[test]
fn an_update_under_a_collation() {
    let s2 = strength(2);
    let _on = collation::activate(Some(&s2));
    let apply = |d: Document, u: Document| update::apply_update(&d, &u, false).unwrap();
    assert_eq!(
        apply(doc! {"a": ["a", "A", "b"]}, doc! {"$pull": {"a": "A"}}),
        doc! {"a": ["b"]}
    );
    assert_eq!(
        apply(doc! {"a": ["a", "b"]}, doc! {"$addToSet": {"a": "A"}}),
        doc! {"a": ["a", "b"]}
    );
    assert_eq!(
        apply(doc! {"s": "a"}, doc! {"$max": {"s": "A"}}),
        doc! {"s": "a"}
    );
    assert_eq!(
        apply(doc! {"s": "a"}, doc! {"$max": {"s": "B"}}),
        doc! {"s": "B"}
    );
    // A plain `$set` that only changes the case still writes.
    assert_eq!(
        apply(doc! {"s": "a"}, doc! {"$set": {"s": "A"}}),
        doc! {"s": "A"}
    );
}
