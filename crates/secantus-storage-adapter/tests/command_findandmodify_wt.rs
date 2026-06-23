//! Real-WiredTiger ports of the `findAndModify` command unit tests. Validators
//! come from real `create` commands; removals are verified with real `count`s.

mod common;

use bson::{doc, Bson};
use common::with_wt;
use secantus_commands::dispatch;

#[test]
fn update_validation_failure_carries_err_info() {
    // findAndModify whose post-apply doc fails the collection validator is
    // rejected with 121 + errInfo (failingDocumentId + details).
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"x": {"$type": "string"}}},
            c,
        );
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": "foo"}]},
            c,
        );
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 1}}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 121);
        let info = reply.get_document("errInfo").unwrap();
        assert_eq!(info.get_i32("failingDocumentId").unwrap(), 1);
        assert!(info.get_document("details").is_ok());
    });
}

#[test]
fn update_passes_validator_when_post_apply_doc_is_valid() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"x": {"$type": "string"}}},
            c,
        );
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": "foo"}]},
            c,
        );
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": "bar"}}},
            c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn update_returns_old_by_default() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]}, c);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 9}}},
            c,
        );
        let value = reply.get_document("value").unwrap();
        assert_eq!(value.get_i32("x").unwrap(), 1, "old image");
        let leo = reply.get_document("lastErrorObject").unwrap();
        assert_eq!(leo.get_i32("n").unwrap(), 1);
        assert!(leo.get_bool("updatedExisting").unwrap());
    });
}

#[test]
fn update_returns_new_when_requested() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]}, c);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 9}}, "new": true},
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("x").unwrap(),
            9,
            "new image"
        );
    });
}

#[test]
fn remove_returns_deleted_doc() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}]},
            c,
        );
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "remove": true},
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id").unwrap(),
            1
        );
        // doc removed: one left.
        assert_eq!(dispatch(&doc! {"count": "c"}, c).get_i32("n").unwrap(), 1);
    });
}

#[test]
fn no_match_returns_null() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 9}, "update": {"$set": {"x": 1}}},
            c,
        );
        assert_eq!(reply.get("value"), Some(&Bson::Null));
        assert_eq!(
            reply
                .get_document("lastErrorObject")
                .unwrap()
                .get_i32("n")
                .unwrap(),
            0
        );
    });
}

#[test]
fn upsert_inserts_and_reports_upserted() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 5}, "update": {"$set": {"x": 1}}, "upsert": true, "new": true},
            c,
        );
        let leo = reply.get_document("lastErrorObject").unwrap();
        assert_eq!(leo.get_i32("upserted").unwrap(), 5);
        assert!(!leo.get_bool("updatedExisting").unwrap());
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("x").unwrap(),
            1
        );
    });
}

#[test]
fn sort_picks_first() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"_id": 1, "g": "a", "p": 3},
                {"_id": 2, "g": "a", "p": 1},
                {"_id": 3, "g": "a", "p": 2},
            ]},
            c,
        );
        // sort by p asc ⇒ _id 2 is the target
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"g": "a"}, "sort": {"p": 1}, "remove": true},
            c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id").unwrap(),
            2
        );
    });
}

#[test]
fn remove_and_update_together_is_failed_to_parse() {
    with_wt(|c| {
        let reply = dispatch(
            &doc! {"findAndModify": "c", "remove": true, "update": {"$set": {"x": 1}}},
            c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
    });
}
