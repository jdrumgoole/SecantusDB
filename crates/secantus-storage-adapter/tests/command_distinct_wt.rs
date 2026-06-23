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
