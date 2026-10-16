//! Real-WiredTiger ports of the `distinct` command unit tests.
//! `dispatch` runs over a real `WtStorage` (via `StorageAdapter`); docs are
//! seeded with real `insert` commands instead of an in-memory fake.

mod common;

use bson::{doc, Bson};
use common::with_wt;
use secantus_commands::dispatch;

fn values(reply: &bson::Document) -> Vec<Bson> {
    reply.get_array("values").unwrap().clone()
}

#[test]
fn distinct_scalar_field() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"x": 1}, {"x": 2}, {"x": 1}, {"x": 3}
            ]},
            c,
        );
        let reply = dispatch(&doc! {"distinct": "c", "key": "x"}, c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let mut v = values(&reply);
        v.sort_by_key(|b| b.as_i32().unwrap());
        assert_eq!(v, vec![Bson::Int32(1), Bson::Int32(2), Bson::Int32(3)]);
    });
}

#[test]
fn distinct_flattens_arrays() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"tags": ["a", "b"]}, {"tags": ["b", "c"]}
            ]},
            c,
        );
        let reply = dispatch(&doc! {"distinct": "c", "key": "tags"}, c);
        let mut v: Vec<String> = values(&reply)
            .iter()
            .map(|b| b.as_str().unwrap().to_string())
            .collect();
        v.sort();
        assert_eq!(v, vec!["a", "b", "c"]);
    });
}

#[test]
fn distinct_with_query_filter() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"g": "a", "x": 1}, {"g": "a", "x": 2}, {"g": "b", "x": 9}
            ]},
            c,
        );
        let reply = dispatch(&doc! {"distinct": "c", "key": "x", "query": {"g": "a"}}, c);
        let mut v = values(&reply);
        v.sort_by_key(|b| b.as_i32().unwrap());
        assert_eq!(v, vec![Bson::Int32(1), Bson::Int32(2)]);
    });
}

#[test]
fn distinct_dotted_key() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"a": {"b": 1}}, {"a": {"b": 2}}]},
            c,
        );
        let reply = dispatch(&doc! {"distinct": "c", "key": "a.b"}, c);
        assert_eq!(values(&reply).len(), 2);
    });
}

#[test]
fn distinct_non_string_key_is_type_mismatch() {
    with_wt(|c| {
        let reply = dispatch(&doc! {"distinct": "c", "key": 1}, c);
        assert_eq!(reply.get_i32("code").unwrap(), 14);
        assert_eq!(reply.get_str("codeName").unwrap(), "TypeMismatch");
    });
}

fn seeded(
    c: &mut secantus_commands::CommandContext,
    docs: Vec<bson::Document>,
    index: bson::Document,
) {
    dispatch(&doc! {"insert": "c", "documents": docs}, c);
    let reply = dispatch(&doc! {"createIndexes": "c", "indexes": [index]}, c);
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply}");
}

/// RM-171: mongod 8.2.11 answers a `distinct` with no query over a multikey
/// index from its DISTINCT_SCAN's keys, each array key unwound once, and a
/// document without the field gives its null key. With a query the index
/// serves no DISTINCT_SCAN and the values are the documents'.
#[test]
fn distinct_over_a_multikey_index_reads_the_keys() {
    with_wt(|c| {
        seeded(
            c,
            vec![
                doc! {"_id": 1, "a": [8, [9]]},
                doc! {"_id": 2, "a": 7},
                doc! {"_id": 3},
            ],
            doc! {"key": {"a": 1}, "name": "a_1"},
        );
        let reply = dispatch(&doc! {"distinct": "c", "key": "a"}, c);
        assert_eq!(
            values(&reply),
            vec![Bson::Null, Bson::Int32(7), Bson::Int32(8), Bson::Int32(9)]
        );
        let reply = dispatch(
            &doc! {"distinct": "c", "key": "a", "query": {"a": {"$gte": 0}}},
            c,
        );
        assert_eq!(
            values(&reply),
            vec![
                Bson::Int32(7),
                Bson::Int32(8),
                Bson::Array(vec![Bson::Int32(9)])
            ]
        );
        // A query that normalises to nothing is still a query.
        let reply = dispatch(
            &doc! {"distinct": "c", "key": "a", "query": {"$and": [{}]}},
            c,
        );
        assert_eq!(
            values(&reply),
            vec![
                Bson::Int32(7),
                Bson::Int32(8),
                Bson::Array(vec![Bson::Int32(9)])
            ]
        );
    });
}

/// RM-172: through an index with a collation, mongod 8.2.11's DISTINCT_SCAN
/// fetches the first document of each key in index order; the null key's
/// first document lacks the field, so no `null`.
#[test]
fn distinct_through_a_collated_index_fetches_each_keys_first_document() {
    with_wt(|c| {
        let collation = doc! {"locale": "en", "strength": 2};
        seeded(
            c,
            vec![
                doc! {"_id": 1, "s": "b"},
                doc! {"_id": 2, "s": "B"},
                doc! {"_id": 3},
                doc! {"_id": 4, "s": Bson::Null},
                doc! {"_id": 5, "s": "a"},
            ],
            doc! {"key": {"s": 1}, "name": "s_en", "collation": collation.clone()},
        );
        let reply = dispatch(
            &doc! {"distinct": "c", "key": "s", "collation": collation.clone()},
            c,
        );
        assert_eq!(values(&reply), vec![Bson::from("a"), Bson::from("b")]);
        let explained = dispatch(
            &doc! {
                "explain": {"distinct": "c", "key": "s", "collation": collation},
                "verbosity": "executionStats",
            },
            c,
        );
        let plan = explained
            .get_document("queryPlanner")
            .and_then(|q| q.get_document("winningPlan"))
            .unwrap();
        let plan = plan.get_document("queryPlan").unwrap_or(plan);
        assert_eq!(plan.get_str("stage").unwrap(), "DISTINCT_SCAN", "{plan}");
        assert!(plan.get_bool("isFetching").unwrap());
        let stats = explained
            .get_document("executionStats")
            .and_then(|s| s.get_document("executionStages"))
            .unwrap();
        assert_eq!(stats.get("docsExamined"), stats.get("nReturned"), "{stats}");
    });
}
