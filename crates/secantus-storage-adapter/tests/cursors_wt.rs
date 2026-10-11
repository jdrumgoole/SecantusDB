//! The cursor commands over real WiredTiger: `find`'s cursor options,
//! `getMore`, `killCursors`, tailable cursors, `count` and `distinct`. Every
//! expectation is what mongod 8.2.11 answered to the same commands
//! (`tools/probes/cursor_commands.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn seed(c: &mut CommandContext) {
    dispatch(
        &doc! {"insert": "c", "documents": [
            {"_id": 1, "v": 5},
            {"_id": 2, "v": 3},
            {"_id": 3, "v": 5},
            {"_id": 4, "v": "x", "n": {"k": 1}},
            {"_id": 5, "v": Bson::Null},
            {"_id": 6},
            {"_id": 7, "v": 2.5},
            {"_id": 8, "v": [3, 9], "n": [{"k": 4}, {"k": 5}]},
        ]},
        c,
    );
    dispatch(
        &doc! {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v_1"}]},
        c,
    );
}

fn cursor(reply: &Document) -> &Document {
    reply
        .get_document("cursor")
        .unwrap_or_else(|_| panic!("no cursor: {reply}"))
}

fn ids(reply: &Document, batch: &str) -> Vec<i32> {
    cursor(reply)
        .get_array(batch)
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

fn code(reply: &Document) -> Option<i32> {
    reply.get_i32("code").ok()
}

/// `singleBatch` returned every document, whatever `batchSize` said.
#[test]
fn single_batch_returns_one_batch_and_no_cursor() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch_full(
            &doc! {"find": "c", "sort": {"_id": 1}, "singleBatch": true, "batchSize": 2},
            c,
        );
        assert_eq!(ids(&reply, "firstBatch"), vec![1, 2]);
        assert_eq!(cursor(&reply).get_i64("id").ok(), Some(0));
        let reply = dispatch_full(
            &doc! {"find": "c", "projection": {"_id": 1}, "sort": {"_id": 1}, "singleBatch": true,
            "batchSize": 3},
            c,
        );
        assert_eq!(ids(&reply, "firstBatch"), vec![1, 2, 3]);
    });
}

#[test]
fn batch_size_zero_opens_a_cursor_even_over_nothing() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch_full(
            &doc! {"find": "c", "filter": {"_id": 99}, "batchSize": 0},
            c,
        );
        let id = cursor(&reply).get_i64("id").unwrap();
        assert_ne!(id, 0, "{reply}");
        let more = dispatch_full(&doc! {"getMore": id, "collection": "c"}, c);
        assert!(ids(&more, "nextBatch").is_empty());
        assert_eq!(cursor(&more).get_i64("id").ok(), Some(0));
        // A collection that does not exist has no cursor to open.
        let reply = dispatch_full(&doc! {"find": "nope", "batchSize": 0}, c);
        assert_eq!(cursor(&reply).get_i64("id").ok(), Some(0));
        // An aggregate opens one all the same.
        let reply = dispatch_full(
            &doc! {"aggregate": "nope", "pipeline": [], "cursor": {"batchSize": 0}},
            c,
        );
        assert_ne!(cursor(&reply).get_i64("id").ok(), Some(0));
    });
}

#[test]
fn find_refuses_what_it_does_not_understand() {
    with_wt(|c| {
        seed(c);
        for (extra, want) in [
            (doc! {"bogus": 1}, 40415),
            (doc! {"batchSize": -1}, 2),
            (doc! {"awaitData": true}, 9),
            (doc! {"noCursorTimeout": 1}, 14),
            (doc! {"hint": {"$natural": 2}}, 2),
            (doc! {"let": {"A": 1}}, 9),
            (doc! {"let": {"a": "$v"}}, 4890500),
            (
                doc! {"min": {"v": 5}, "max": {"v": 3}, "hint": {"v": 1}},
                51175,
            ),
            (doc! {"min": {"v": 3}}, 291),
            (doc! {"min": {"w": 3}, "hint": {"v": 1}}, 51174),
        ] {
            let mut cmd = doc! {"find": "c"};
            cmd.extend(extra.clone());
            let reply = dispatch_full(&cmd, c);
            assert_eq!(code(&reply), Some(want), "{extra}: {reply}");
        }
        // An empty hint is no hint.
        let reply = dispatch_full(&doc! {"find": "c", "filter": {"v": 5}, "hint": {}}, c);
        assert_eq!(ids(&reply, "firstBatch").len(), 2, "{reply}");
        // The collection a command names is a non-empty string.
        for name in ["find", "count", "distinct", "insert", "drop", "listIndexes"] {
            let reply = dispatch(&doc! {name: 5}, c);
            assert_eq!(
                reply.get_str("errmsg").ok(),
                Some("Collection name has invalid type int"),
                "{name}"
            );
            assert_eq!(code(&dispatch(&doc! {name: ""}, c)), Some(73), "{name}");
        }
    });
}

/// `min` and `max` bound the hinted index's KEYS. A document whose array has
/// one element inside the bounds is returned, and so is one that lacks the
/// field when null is inside them; both were left out.
#[test]
fn min_and_max_bound_index_keys() {
    with_wt(|c| {
        seed(c);
        let mut below = ids(
            &dispatch_full(&doc! {"find": "c", "max": {"v": 5}, "hint": {"v": 1}}, c),
            "firstBatch",
        );
        below.sort_unstable();
        assert_eq!(below, vec![2, 5, 6, 7, 8]);
        let mut between = ids(
            &dispatch_full(
                &doc! {"find": "c", "min": {"v": 3}, "max": {"v": 5}, "hint": {"v": 1}},
                c,
            ),
            "firstBatch",
        );
        between.sort_unstable();
        assert_eq!(between, vec![2, 8]);
    });
}

#[test]
fn get_more_checks_its_arguments_before_the_cursor() {
    with_wt(|c| {
        seed(c);
        let open = dispatch_full(&doc! {"find": "c", "sort": {"_id": 1}, "batchSize": 2}, c);
        let id = cursor(&open).get_i64("id").unwrap();
        for (cmd, want) in [
            (doc! {"getMore": id as i32, "collection": "c"}, 14),
            (doc! {"getMore": id, "collection": "nope"}, 13),
            (doc! {"getMore": id, "collection": ""}, 73),
            (doc! {"getMore": id, "collection": "c", "batchSize": -1}, 2),
            (doc! {"getMore": id, "collection": "c", "maxTimeMS": 100}, 2),
            (doc! {"getMore": id, "collection": "c", "bogus": 1}, 40415),
            (doc! {"getMore": id}, 40414),
        ] {
            let reply = dispatch_full(&cmd, c);
            assert_eq!(code(&reply), Some(want), "{cmd}: {reply}");
        }
        // None of those consumed anything.
        let more = dispatch_full(&doc! {"getMore": id, "collection": "c", "batchSize": 2}, c);
        assert_eq!(ids(&more, "nextBatch"), vec![3, 4]);
        // killCursors takes int64 ids.
        let reply = dispatch(&doc! {"killCursors": "c", "cursors": [1]}, c);
        assert_eq!(code(&reply), Some(14), "{reply}");
        let reply = dispatch(&doc! {"killCursors": "c", "cursors": [id]}, c);
        assert_eq!(reply.get_array("cursorsKilled").map(Vec::len).ok(), Some(1));
    });
}

/// A cursor whose collection is dropped or renamed says so on its next
/// `getMore` (175), once.
#[test]
fn a_cursor_is_told_why_its_collection_went_away() {
    with_wt(|c| {
        seed(c);
        let open = |c: &mut CommandContext| {
            let reply = dispatch_full(&doc! {"find": "c", "batchSize": 2}, c);
            cursor(&reply).get_i64("id").unwrap()
        };
        let id = open(c);
        let renamed = {
            let db = std::mem::replace(&mut c.db_name, "admin".into());
            let reply = dispatch(&doc! {"renameCollection": "t.c", "to": "t.d"}, c);
            c.db_name = db;
            reply
        };
        assert_eq!(renamed.get_f64("ok").ok(), Some(1.0), "{renamed}");
        let reply = dispatch_full(&doc! {"getMore": id, "collection": "c"}, c);
        assert_eq!(code(&reply), Some(175), "{reply}");
        assert!(reply
            .get_str("errmsg")
            .unwrap()
            .starts_with("collection renamed from 't.c' to 't.d'. UUID "));
        assert_eq!(
            code(&dispatch_full(&doc! {"getMore": id, "collection": "c"}, c)),
            Some(43)
        );
        seed(c);
        let id = open(c);
        dispatch(&doc! {"dropDatabase": 1}, c);
        let reply = dispatch_full(&doc! {"getMore": id, "collection": "c"}, c);
        assert_eq!(code(&reply), Some(175), "{reply}");
        assert!(reply
            .get_str("errmsg")
            .unwrap()
            .starts_with("collection dropped. UUID "));
    });
}

/// A tailable cursor keeps its filter and projection for the documents that
/// arrive later. It applied them to the first batch only.
#[test]
fn a_tailable_cursor_keeps_its_filter_and_projection() {
    with_wt(|c| {
        dispatch(&doc! {"create": "cap", "capped": true, "size": 4096}, c);
        dispatch(
            &doc! {"insert": "cap", "documents": [{"_id": 1, "a": 1}, {"_id": 3, "a": 3}]},
            c,
        );
        let open = dispatch_full(
            &doc! {"find": "cap", "filter": {"_id": {"$gte": 3}}, "projection": {"a": 0},
            "tailable": true},
            c,
        );
        assert_eq!(
            cursor(&open).get_array("firstBatch").ok(),
            Some(&vec![Bson::Document(doc! {"_id": 3})])
        );
        let id = cursor(&open).get_i64("id").unwrap();
        dispatch(
            &doc! {"insert": "cap", "documents": [{"_id": 4, "a": 4}, {"_id": 0, "a": 0}]},
            c,
        );
        let more = dispatch_full(&doc! {"getMore": id, "collection": "cap"}, c);
        assert_eq!(
            cursor(&more).get_array("nextBatch").ok(),
            Some(&vec![Bson::Document(doc! {"_id": 4})]),
            "{more}"
        );
        assert!(
            !cursor(&more).contains_key("postBatchResumeToken"),
            "{more}"
        );
        // What a tailable cursor refuses, and where it has nothing to hold.
        for (extra, want) in [
            (doc! {"sort": {"_id": 1}}, 2),
            (doc! {"singleBatch": true}, 2),
        ] {
            let mut cmd = doc! {"find": "cap", "tailable": true};
            cmd.extend(extra);
            assert_eq!(code(&dispatch_full(&cmd, c)), Some(want));
        }
        dispatch(&doc! {"create": "empty", "capped": true, "size": 4096}, c);
        for name in ["empty", "nope"] {
            let reply = dispatch_full(&doc! {"find": name, "tailable": true}, c);
            assert_eq!(
                cursor(&reply).get_i64("id").ok(),
                Some(0),
                "{name}: {reply}"
            );
        }
    });
}

#[test]
fn count_reads_limit_and_skip_as_mongod_does() {
    with_wt(|c| {
        seed(c);
        let n = |c: &mut CommandContext, extra: Document| {
            let mut cmd = doc! {"count": "c"};
            cmd.extend(extra);
            dispatch(&cmd, c)
        };
        assert_eq!(n(c, doc! {"limit": -3}).get_i32("n").ok(), Some(3));
        assert_eq!(n(c, doc! {"hint": {}}).get_i32("n").ok(), Some(8));
        assert_eq!(code(&n(c, doc! {"skip": -1})), Some(2));
        assert_eq!(code(&n(c, doc! {"limit": 2.5})), Some(9));
        assert_eq!(code(&n(c, doc! {"bogus": 1})), Some(40415));
    });
}

/// "Distinct" is BSON equality, and the values come back sorted. `==` on the
/// values kept one of each TYPE: 1, 1.0 and a Decimal128 1 were three values.
#[test]
fn distinct_values_are_distinct_by_value_and_sorted() {
    with_wt(|c| {
        seed(c);
        let values = |c: &mut CommandContext, cmd: Document| {
            dispatch(&cmd, c).get_array("values").cloned().unwrap()
        };
        assert_eq!(
            values(c, doc! {"distinct": "c", "key": "v"}),
            vec![
                Bson::Null,
                Bson::Double(2.5),
                Bson::Int32(3),
                Bson::Int32(5),
                Bson::Int32(9),
                Bson::String("x".into())
            ]
        );
        // Through an array of documents.
        assert_eq!(
            values(c, doc! {"distinct": "c", "key": "n.k"}),
            vec![Bson::Int32(1), Bson::Int32(4), Bson::Int32(5)]
        );
        dispatch(
            &doc! {"insert": "m", "documents": [
                {"a": 1}, {"a": 1.0}, {"a": 1_i64}, {"a": 0}, {"a": -0.0},
                {"a": f64::NAN}, {"a": f64::NAN}, {"a": {"x": 1}}, {"a": {"x": 1.0}},
            ]},
            c,
        );
        let got = values(c, doc! {"distinct": "m", "key": "a"});
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(matches!(got[0], Bson::Double(n) if n.is_nan()), "{got:?}");
        assert_eq!(&got[1..3], &[Bson::Int32(0), Bson::Int32(1)]);
        let reply = dispatch(&doc! {"distinct": "c", "key": "v", "sort": 1}, c);
        assert_eq!(code(&reply), Some(40415), "{reply}");
    });
}
