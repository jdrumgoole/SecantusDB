//! Change streams over real WiredTiger: what `$changeStream` accepts, the
//! events it delivers, resuming, and what a transaction's events carry.
//! Every expectation is what mongod 8.2.11 answered to the same commands
//! (`tools/probes/change_stream_commands.py`).

mod common;

use bson::{doc, Document};
use common::{dispatch_admin, dispatch_full, with_transactions, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn watch(c: &mut CommandContext, spec: Document) -> Document {
    c.replica_set_name = Some("rs0".into());
    dispatch_full(
        &doc! {"aggregate": "c", "pipeline": [{"$changeStream": spec}], "cursor": {}},
        c,
    )
}

fn cursor_id(reply: &Document) -> i64 {
    reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("no cursor: {reply}"))
        .get_i64("id")
        .unwrap()
}

fn more(c: &mut CommandContext, id: i64) -> Document {
    dispatch_full(
        &doc! {"getMore": id, "collection": "c", "maxTimeMS": 200},
        c,
    )
}

fn events(reply: &Document) -> Vec<Document> {
    let cursor = reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("{reply}"));
    cursor
        .get_array("nextBatch")
        .or_else(|_| cursor.get_array("firstBatch"))
        .unwrap()
        .iter()
        .map(|e| e.as_document().unwrap().clone())
        .collect()
}

fn lsid() -> Document {
    doc! {"id": bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![3; 16] }}
}

/// A write in a transaction names it, ahead of the resume token.
#[test]
fn transaction_events_carry_the_session() {
    with_wt(|c| {
        with_transactions(c);
        let opened = watch(c, doc! {});
        let id = cursor_id(&opened);
        let tx = doc! {"lsid": lsid(), "txnNumber": 1_i64, "autocommit": false};
        let mut first = doc! {"insert": "c", "documents": [{"_id": 1}]};
        first.extend(tx.clone());
        first.insert("startTransaction", true);
        let reply = dispatch(&first, c);
        assert_eq!(reply.get_i32("n").ok(), Some(1), "{reply}");
        let mut second =
            doc! {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]};
        second.extend(tx.clone());
        dispatch(&second, c);
        let mut commit = doc! {"commitTransaction": 1};
        commit.extend(tx);
        let reply = dispatch_admin(&commit, c);
        assert_eq!(reply.get_f64("ok").ok(), Some(1.0), "{reply}");
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 2}]}, c);
        let mut got = Vec::new();
        for _ in 0..5 {
            got.extend(events(&more(c, id)));
            if got.len() >= 3 {
                break;
            }
        }
        assert_eq!(got.len(), 3, "{got:?}");
        for event in &got[..2] {
            let keys: Vec<&str> = event.keys().map(String::as_str).collect();
            assert_eq!(&keys[..3], &["txnNumber", "lsid", "_id"], "{event}");
            assert_eq!(event.get_i64("txnNumber").ok(), Some(1));
            let session = event.get_document("lsid").unwrap();
            assert_eq!(session.get("id"), lsid().get("id"));
            assert!(session.contains_key("uid"), "{event}");
        }
        assert!(!got[2].contains_key("txnNumber"), "{}", got[2]);
    });
}

fn code(reply: &Document) -> Option<i32> {
    reply.get_i32("code").ok()
}

/// What `$changeStream` refuses, with mongod's codes.
#[test]
fn the_spec_is_checked() {
    with_wt(|c| {
        assert_eq!(code(&watch(c, doc! {"bogus": 1})), Some(40415));
        assert_eq!(
            code(&watch(c, doc! {"fullDocument": "everything"})),
            Some(2)
        );
        assert_eq!(code(&watch(c, doc! {"fullDocument": 1})), Some(14));
        assert_eq!(code(&watch(c, doc! {"showExpandedEvents": 1})), Some(14));
        assert_eq!(code(&watch(c, doc! {"resumeAfter": "abc"})), Some(14));
        assert_eq!(
            code(&watch(c, doc! {"resumeAfter": {"_data": "zz"}})),
            Some(9)
        );
        assert_eq!(
            code(&watch(c, doc! {"resumeAfter": {"_data": 1}})),
            Some(40647)
        );
        assert_eq!(
            code(&watch(
                c,
                doc! {"resumeAfter": {"_data": "82"}, "startAfter": {"_data": "82"}}
            )),
            Some(50865)
        );
        assert_eq!(code(&watch(c, doc! {"startAtOperationTime": 5})), Some(14));
        assert_eq!(
            code(&watch(c, doc! {"allChangesForCluster": true})),
            Some(72)
        );
        c.replica_set_name = Some("rs0".into());
        let stage_not_first = dispatch_full(
            &doc! {"aggregate": "c", "pipeline": [{"$match": {}}, {"$changeStream": {}}], "cursor": {}},
            c,
        );
        assert_eq!(code(&stage_not_first), Some(40602), "{stage_not_first}");
        let sorted = dispatch_full(
            &doc! {"aggregate": "c", "pipeline": [{"$changeStream": {}}, {"$sort": {"_id": 1}}], "cursor": {}},
            c,
        );
        assert_eq!(code(&sorted), Some(20), "{sorted}");
        assert_eq!(
            sorted.get_str("errmsg").ok(),
            Some("$sort is not permitted in a $changeStream pipeline")
        );
        let split_alone = dispatch_full(
            &doc! {"aggregate": "c", "pipeline": [{"$changeStreamSplitLargeEvent": {}}], "cursor": {}},
            c,
        );
        assert_eq!(code(&split_alone), Some(20), "{split_alone}");
        let on_system = dispatch_full(
            &doc! {"aggregate": "system.views", "pipeline": [{"$changeStream": {}}], "cursor": {}},
            c,
        );
        assert_eq!(code(&on_system), Some(73), "{on_system}");
        c.db_name = "local".into();
        assert_eq!(code(&watch(c, doc! {})), Some(73));
    });
}

/// The cursor document's field order, and `invalidated` when the stream
/// ended; a closed cursor's id is gone.
#[test]
fn an_invalidated_stream_says_so_and_is_gone() {
    with_wt(|c| {
        let opened = watch(c, doc! {});
        let keys: Vec<&str> = opened
            .get_document("cursor")
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["firstBatch", "postBatchResumeToken", "id", "ns"]);
        let id = cursor_id(&opened);
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        dispatch(&doc! {"drop": "c"}, c);
        let reply = more(c, id);
        let cursor = reply.get_document("cursor").unwrap();
        let keys: Vec<&str> = cursor.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "nextBatch",
                "postBatchResumeToken",
                "invalidated",
                "id",
                "ns"
            ],
            "{reply}"
        );
        assert_eq!(cursor.get_i64("id").ok(), Some(0));
        let types: Vec<&str> = events(&reply)
            .iter()
            .map(|e| e.get_str("operationType").unwrap().to_string())
            .map(|s| Box::leak(s.into_boxed_str()) as &str)
            .collect();
        assert_eq!(types, vec!["insert", "drop", "invalidate"]);
        assert_eq!(code(&more(c, id)), Some(43));
        assert_eq!(code(&more(c, 0)), Some(43));
    });
}

/// Resuming re-reads the token's event, which must be the first one seen.
#[test]
fn a_resume_token_must_be_found() {
    with_wt(|c| {
        let id = cursor_id(&watch(c, doc! {}));
        for i in 1..=3 {
            dispatch(&doc! {"insert": "c", "documents": [{"_id": i}]}, c);
        }
        // The oplog may be drained a little behind the writes.
        let mut got = Vec::new();
        for _ in 0..5 {
            got.extend(events(&more(c, id)));
            if got.len() >= 3 {
                break;
            }
        }
        assert_eq!(got.len(), 3);
        let token = got[0].get_document("_id").unwrap().clone();
        // After the first event: the other two.
        let resumed = watch(c, doc! {"resumeAfter": token.clone()});
        let rid = cursor_id(&resumed);
        let mut after = events(&resumed);
        for _ in 0..5 {
            if after.len() >= 2 {
                break;
            }
            after.extend(events(&more(c, rid)));
        }
        let ids: Vec<i32> = after
            .iter()
            .map(|e| {
                e.get_document("documentKey")
                    .unwrap()
                    .get_i32("_id")
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, vec![2, 3], "{resumed}");
        // A pipeline that drops the token's event cannot resume.
        c.replica_set_name = Some("rs0".into());
        let filtered = dispatch_full(
            &doc! {"aggregate": "c", "pipeline": [
                {"$changeStream": {"resumeAfter": token.clone()}},
                {"$match": {"documentKey._id": 3}}
            ], "cursor": {}},
            c,
        );
        assert_eq!(code(&filtered), Some(280), "{filtered}");
        let keys: Vec<&str> = filtered.keys().map(String::as_str).collect();
        assert_eq!(keys[0], "errorLabels");
        assert!(filtered
            .get_str("errmsg")
            .unwrap()
            .starts_with("Executor error during aggregate command on namespace: t.c :: caused by :: cannot resume stream; the resume token was not found."));
        // On another collection the token's event never comes: fatal at getMore.
        let other = dispatch_full(
            &doc! {"aggregate": "d", "pipeline": [{"$changeStream": {"resumeAfter": token}}], "cursor": {}},
            c,
        );
        let other_id = cursor_id(&other);
        dispatch(&doc! {"insert": "d", "documents": [{"_id": 1}]}, c);
        let reply = dispatch_full(
            &doc! {"getMore": other_id, "collection": "d", "maxTimeMS": 200},
            c,
        );
        assert_eq!(code(&reply), Some(280), "{reply}");
    });
}

/// `startAtOperationTime` in the future delivers nothing before it; an
/// implicit create is an event; a write in the internal databases is not a
/// cluster event; a small event is not split.
#[test]
fn start_time_create_and_split() {
    with_wt(|c| {
        let far = bson::Timestamp {
            time: i32::MAX as u32,
            increment: 1,
        };
        let id = cursor_id(&watch(c, doc! {"startAtOperationTime": far}));
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        assert!(events(&more(c, id)).is_empty());
        let id = cursor_id(&watch(c, doc! {"showExpandedEvents": true}));
        dispatch(&doc! {"insert": "n", "documents": [{"_id": 1}]}, c);
        dispatch(&doc! {"drop": "n"}, c);
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 2}]}, c);
        let got = events(&more(c, id));
        assert_eq!(got.len(), 1, "{got:?}");
        let expanded = dispatch_full(
            &doc! {"aggregate": "e", "pipeline": [{"$changeStream": {"showExpandedEvents": true}}], "cursor": {}},
            c,
        );
        let eid = cursor_id(&expanded);
        dispatch(&doc! {"insert": "e", "documents": [{"_id": 1}]}, c);
        let got = dispatch_full(
            &doc! {"getMore": eid, "collection": "e", "maxTimeMS": 200},
            c,
        );
        let types: Vec<String> = events(&got)
            .iter()
            .map(|e| e.get_str("operationType").unwrap().to_string())
            .collect();
        assert_eq!(types, vec!["create", "insert"], "{got}");
        assert_eq!(events(&got)[0].get_str("nsType").ok(), Some("collection"));
        let split = dispatch_full(
            &doc! {"aggregate": "c", "pipeline": [{"$changeStream": {}}, {"$changeStreamSplitLargeEvent": {}}], "cursor": {}},
            c,
        );
        let sid = cursor_id(&split);
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 3}]}, c);
        let got = events(&more(c, sid));
        assert!(!got[0].contains_key("splitEvent"), "{}", got[0]);
    });
}
