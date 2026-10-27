//! Sessions, multi-document transactions and retryable writes over real
//! WiredTiger. Every expectation here is what mongod 8.2.11 answered to the
//! same commands (`tools/probes/transactions.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_admin, dispatch_full, with_transactions, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn lsid(n: u8) -> Document {
    doc! {"id": bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: vec![n; 16] }}
}

/// `cmd` as a statement of transaction `number` on session `s`.
fn tx(c: &mut CommandContext, s: u8, number: i64, start: bool, mut cmd: Document) -> Document {
    cmd.insert("lsid", lsid(s));
    cmd.insert("txnNumber", number);
    cmd.insert("autocommit", false);
    if start {
        cmd.insert("startTransaction", true);
    }
    dispatch_full(&cmd, c)
}

fn commit(c: &mut CommandContext, s: u8, number: i64) -> Document {
    dispatch_admin(
        &doc! {"commitTransaction": 1, "lsid": lsid(s), "txnNumber": number, "autocommit": false},
        c,
    )
}

/// `cmd` as a retryable write `number` on session `s`.
fn retry(c: &mut CommandContext, s: u8, number: i64, mut cmd: Document) -> Document {
    cmd.insert("lsid", lsid(s));
    cmd.insert("txnNumber", number);
    dispatch(&cmd, c)
}

fn ids(c: &mut CommandContext, coll: &str) -> Vec<i32> {
    let reply = dispatch_full(&doc! {"find": coll, "sort": {"_id": 1}}, c);
    reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| d.as_document().unwrap().get_i32("_id").unwrap())
        .collect()
}

fn value(c: &mut CommandContext, id: i32) -> Option<Bson> {
    let reply = dispatch_full(&doc! {"find": "c", "filter": {"_id": id}}, c);
    let batch = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap();
    batch.first()?.as_document()?.get("v").cloned()
}

fn code(reply: &Document) -> Option<i32> {
    reply.get_i32("code").ok()
}

fn seed(c: &mut CommandContext) {
    with_transactions(c);
    c.replica_set_name = Some("rs0".into());
    dispatch(
        &doc! {"insert": "c", "documents": [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}]},
        c,
    );
}

/// The session fields are checked before anything is written. Each of these
/// used to run the insert.
#[test]
fn a_malformed_transaction_envelope_writes_nothing() {
    with_wt(|c| {
        seed(c);
        let insert = doc! {"insert": "c", "documents": [{"_id": 3}]};
        for (extra, want) in [
            (doc! {"txnNumber": 1_i64}, 72),
            (doc! {"lsid": lsid(1), "txnNumber": 1_i32}, 14),
            (
                doc! {"lsid": lsid(1), "txnNumber": 1_i64, "autocommit": true},
                72,
            ),
            (
                doc! {"lsid": lsid(1), "txnNumber": 1_i64, "startTransaction": true},
                72,
            ),
            (doc! {"lsid": lsid(1), "autocommit": false}, 72),
            (doc! {"lsid": {"id": 1}}, 14),
            (doc! {"lsid": lsid(1), "txnNumber": -1_i64}, 72),
        ] {
            let mut cmd = insert.clone();
            cmd.extend(extra.clone());
            let reply = dispatch(&cmd, c);
            assert_eq!(code(&reply), Some(want), "{extra}: {reply}");
        }
        assert_eq!(ids(c, "c"), vec![1, 2]);
        // A transaction number on a command that is not a write.
        let reply = retry(c, 1, 1, doc! {"find": "c"});
        assert_eq!(code(&reply), Some(50768), "{reply}");
    });
}

/// A command a transaction does not allow is refused without touching the
/// transaction; a statement that fails while running aborts it.
#[test]
fn a_refused_command_leaves_the_transaction_open_and_a_failed_one_aborts_it() {
    with_wt(|c| {
        seed(c);
        // First statement: nothing starts.
        let reply = tx(c, 1, 1, true, doc! {"count": "c"});
        assert_eq!(code(&reply), Some(263), "{reply}");
        let reply = tx(c, 1, 1, false, doc! {"find": "c"});
        assert_eq!(code(&reply), Some(251));
        assert!(
            reply
                .get_str("errmsg")
                .unwrap()
                .ends_with("The active transaction number is -1"),
            "{reply}"
        );
        // Later statement: the transaction carries on and commits.
        tx(
            c,
            1,
            2,
            true,
            doc! {"insert": "c", "documents": [{"_id": 3}]},
        );
        for refused in [
            doc! {"count": "c"},
            doc! {"listCollections": 1},
            doc! {"ping": 1},
        ] {
            assert_eq!(code(&tx(c, 1, 2, false, refused)), Some(263));
        }
        assert_eq!(commit(c, 1, 2).get_f64("ok").ok(), Some(1.0));
        assert_eq!(ids(c, "c"), vec![1, 2, 3]);
        // A statement that fails aborts, and every later request says so.
        tx(
            c,
            1,
            3,
            true,
            doc! {"insert": "c", "documents": [{"_id": 4}]},
        );
        let dup = tx(
            c,
            1,
            3,
            false,
            doc! {"insert": "c", "documents": [{"_id": 1}]},
        );
        assert!(dup.contains_key("writeErrors"), "{dup}");
        let reply = commit(c, 1, 3);
        assert_eq!(
            reply.get_str("errmsg").ok(),
            Some("Transaction with { txnNumber: 3 } has been aborted."),
            "{reply}"
        );
        assert_eq!(reply.keys().next().map(String::as_str), Some("errorLabels"));
        assert_eq!(ids(c, "c"), vec![1, 2, 3]);
        // An unordered batch stops at its first error too.
        let reply = tx(
            c,
            1,
            4,
            true,
            doc! {"insert": "c", "documents": [{"_id": 5}, {"_id": 1}, {"_id": 6}], "ordered": false},
        );
        assert_eq!(reply.get_i32("n").ok(), Some(1), "{reply}");
    });
}

#[test]
fn a_transaction_is_ended_on_admin_and_only_once_it_exists() {
    with_wt(|c| {
        seed(c);
        tx(
            c,
            1,
            1,
            true,
            doc! {"insert": "c", "documents": [{"_id": 3}]},
        );
        let outside = dispatch(
            &doc! {"commitTransaction": 1, "lsid": lsid(1), "txnNumber": 1_i64, "autocommit": false},
            c,
        );
        assert_eq!(code(&outside), Some(13), "{outside}");
        assert_eq!(
            code(&dispatch_admin(&doc! {"commitTransaction": 1}, c)),
            Some(125)
        );
        assert_eq!(commit(c, 1, 1).get_f64("ok").ok(), Some(1.0));
        // Committed: a second commit is fine, a restart at the number is not.
        assert_eq!(commit(c, 1, 1).get_f64("ok").ok(), Some(1.0));
        let again = tx(c, 1, 1, true, doc! {"find": "c"});
        assert_eq!(code(&again), Some(117), "{again}");
        let older = tx(c, 1, 0, true, doc! {"find": "c"});
        assert_eq!(code(&older), Some(225), "{older}");
    });
}

/// A collection created inside a transaction is held to its own validator by
/// the statements that follow. The validator was read on a session that could
/// not see the uncommitted `create`, so the insert went in unchecked.
#[test]
fn a_validator_created_in_a_transaction_is_enforced_in_it() {
    with_wt(|c| {
        seed(c);
        let made = tx(
            c,
            1,
            1,
            true,
            doc! {"create": "n", "validator": {"v": {"$gte": 0}}},
        );
        assert_eq!(made.get_f64("ok").ok(), Some(1.0), "{made}");
        let bad = tx(
            c,
            1,
            1,
            false,
            doc! {"insert": "n", "documents": [{"_id": 1, "v": -1}]},
        );
        let errors = bad.get_array("writeErrors").expect("refused");
        assert_eq!(
            errors[0].as_document().unwrap().get_i32("code").ok(),
            Some(121)
        );
        assert_eq!(code(&commit(c, 1, 1)), Some(251));
        assert!(ids(c, "n").is_empty());
    });
}

#[test]
fn what_a_transaction_may_create() {
    with_wt(|c| {
        seed(c);
        for (number, (cmd, want)) in [
            (doc! {"create": "c"}, 48),
            (doc! {"create": "vw", "viewOn": "c", "pipeline": []}, 263),
            (doc! {"create": "ts", "timeseries": {"timeField": "t"}}, 263),
            (
                doc! {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v"}]},
                263,
            ),
            (
                doc! {"aggregate": "c", "pipeline": [{"$unionWith": "c"}], "cursor": {}},
                263,
            ),
            (doc! {"find": "c", "tailable": true}, 72),
            (
                doc! {"insert": "system.js", "documents": [{"_id": 1}]},
                50791,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let reply = tx(c, 9, number as i64, true, cmd.clone());
            assert_eq!(code(&reply), Some(want), "{cmd}: {reply}");
        }
        // An index on a collection the transaction itself makes is fine while
        // it is empty.
        assert_eq!(
            tx(c, 2, 1, true, doc! {"create": "n"}).get_f64("ok").ok(),
            Some(1.0)
        );
        let built = tx(
            c,
            2,
            1,
            false,
            doc! {"createIndexes": "n", "indexes": [{"key": {"v": 1}, "name": "v"}]},
        );
        assert_eq!(
            built.get_bool("createdCollectionAutomatically").ok(),
            Some(true),
            "{built}"
        );
        tx(
            c,
            2,
            1,
            false,
            doc! {"insert": "n", "documents": [{"_id": 1}]},
        );
        let late = tx(
            c,
            2,
            1,
            false,
            doc! {"createIndexes": "n", "indexes": [{"key": {"w": 1}, "name": "w"}]},
        );
        assert_eq!(
            late.get_str("errmsg").ok(),
            Some(
                "Cannot create new indexes on non-empty collection t.n in a multi-document \
                 transaction."
            ),
            "{late}"
        );
    });
}

/// A retried statement is answered, not run again -- and only a statement
/// that wrote is remembered.
#[test]
fn a_retryable_write_is_applied_once_per_statement() {
    with_wt(|c| {
        seed(c);
        let inc = doc! {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"v": 1}}}]};
        let first = retry(c, 1, 1, inc.clone());
        assert!(!first.contains_key("retriedStmtIds"), "{first}");
        let again = retry(c, 1, 1, inc.clone());
        assert_eq!(
            again.get_array("retriedStmtIds").ok(),
            Some(&vec![Bson::Int32(0)])
        );
        assert_eq!(again.get_i32("nModified").ok(), Some(1));
        assert_eq!(value(c, 1), Some(Bson::Int32(2)));
        // A new number runs it again; the old number is then too old.
        retry(c, 1, 2, inc.clone());
        assert_eq!(value(c, 1), Some(Bson::Int32(3)));
        assert_eq!(code(&retry(c, 1, 1, inc.clone())), Some(225));

        // A batch that failed part-way: the retry answers for what went in,
        // and tries the rest again.
        let batch = doc! {"insert": "c", "documents": [{"_id": 3}, {"_id": 1}, {"_id": 5}]};
        let first = retry(c, 2, 1, batch.clone());
        assert_eq!(first.get_i32("n").ok(), Some(1), "{first}");
        let again = retry(c, 2, 1, batch.clone());
        assert_eq!(again.get_i32("n").ok(), Some(1), "{again}");
        assert_eq!(
            again.get_array("retriedStmtIds").ok(),
            Some(&vec![Bson::Int32(0)])
        );
        let error = again.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(error.get_i32("index").ok(), Some(1), "{again}");
        assert_eq!(ids(c, "c"), vec![1, 2, 3]);

        // Statement ids say which statements are the same.
        let reply = retry(
            c,
            3,
            1,
            doc! {"insert": "c", "documents": [{"_id": 10}, {"_id": 11}], "stmtIds": [5, 6]},
        );
        assert_eq!(reply.get_i32("n").ok(), Some(2));
        let reply = retry(
            c,
            3,
            1,
            doc! {"insert": "c", "documents": [{"_id": 12}, {"_id": 11}], "stmtIds": [7, 6]},
        );
        assert_eq!(reply.get_i32("n").ok(), Some(2), "{reply}");
        assert_eq!(
            reply.get_array("retriedStmtIds").ok(),
            Some(&vec![Bson::Int32(6)])
        );
        assert_eq!(ids(c, "c"), vec![1, 2, 3, 10, 11, 12]);

        // An update that changed nothing is not remembered: it runs again.
        let same = doc! {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$set": {"v": 2}}}]};
        retry(c, 4, 1, same.clone());
        dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$set": {"v": 7}}}]},
            c,
        );
        let again = retry(c, 4, 1, same);
        assert!(!again.contains_key("retriedStmtIds"), "{again}");
        assert_eq!(value(c, 2), Some(Bson::Int32(2)));
    });
}

#[test]
fn a_retried_find_and_modify_gives_its_first_answer() {
    with_wt(|c| {
        seed(c);
        let fam = doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$inc": {"v": 1}},
        "new": true};
        let first = retry(c, 1, 1, fam.clone());
        let again = retry(c, 1, 1, fam.clone());
        assert_eq!(
            first.get_document("value").ok(),
            again.get_document("value").ok()
        );
        assert_eq!(again.get_i32("retriedStmtId").ok(), Some(0), "{again}");
        assert_eq!(value(c, 1), Some(Bson::Int32(2)));
        // A findAndModify cannot be answered from an insert's record.
        retry(c, 2, 1, doc! {"insert": "c", "documents": [{"_id": 3}]});
        assert_eq!(code(&retry(c, 2, 1, fam)), Some(40608));
    });
}

#[test]
fn a_retryable_write_refuses_what_cannot_be_retried() {
    with_wt(|c| {
        seed(c);
        let many = retry(
            c,
            1,
            1,
            doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"v": 9}}, "multi": true}]},
        );
        let error = many.get_array("writeErrors").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(
            error.get_str("errmsg").ok(),
            Some("Cannot use (or request) retryable writes with multi=true"),
            "{many}"
        );
        assert_eq!(value(c, 1), Some(Bson::Int32(1)));
        let all = retry(
            c,
            1,
            2,
            doc! {"delete": "c", "deletes": [{"q": {}, "limit": 0_i32}]},
        );
        assert!(all.contains_key("writeErrors"), "{all}");
        assert_eq!(ids(c, "c"), vec![1, 2]);
        // A retryable write at the number of a transaction.
        tx(c, 2, 1, true, doc! {"find": "c"});
        let reply = retry(c, 2, 1, doc! {"insert": "c", "documents": [{"_id": 3}]});
        assert_eq!(code(&reply), Some(217), "{reply}");
    });
}

/// A cursor is read on only by the session, and the transaction, it was
/// opened in.
#[test]
fn a_cursor_belongs_to_the_session_that_opened_it() {
    with_wt(|c| {
        seed(c);
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 3}, {"_id": 4}, {"_id": 5}, {"_id": 6}]},
            c,
        );
        let open = |c: &mut CommandContext, mut cmd: Document| -> i64 {
            cmd.insert("batchSize", 2_i32);
            dispatch_full(&cmd, c)
                .get_document("cursor")
                .unwrap()
                .get_i64("id")
                .unwrap()
        };
        let more = |c: &mut CommandContext, id: i64, extra: Document| -> Document {
            let mut cmd = doc! {"getMore": id, "collection": "c", "batchSize": 1_i32};
            cmd.extend(extra);
            dispatch_full(&cmd, c)
        };
        let mine = open(c, doc! {"find": "c", "lsid": lsid(1)});
        assert_eq!(code(&more(c, mine, doc! {})), Some(50737));
        assert_eq!(code(&more(c, mine, doc! {"lsid": lsid(2)})), Some(50738));
        assert_eq!(
            more(c, mine, doc! {"lsid": lsid(1)}).get_f64("ok").ok(),
            Some(1.0)
        );
        let nobodys = open(c, doc! {"find": "c"});
        assert_eq!(code(&more(c, nobodys, doc! {"lsid": lsid(1)})), Some(50736));
        // Inside a transaction, and out of it.
        let inside = tx(c, 3, 1, true, doc! {"find": "c", "batchSize": 2_i32})
            .get_document("cursor")
            .unwrap()
            .get_i64("id")
            .unwrap();
        assert_eq!(code(&more(c, inside, doc! {"lsid": lsid(3)})), Some(50740));
        assert_eq!(
            tx(
                c,
                3,
                1,
                false,
                doc! {"getMore": inside, "collection": "c", "batchSize": 1_i32}
            )
            .get_f64("ok")
            .ok(),
            Some(1.0)
        );
        // Using another cursor in the transaction fails, and aborts it.
        let stray = tx(c, 3, 1, false, doc! {"getMore": mine, "collection": "c"});
        assert_eq!(code(&stray), Some(50738), "{stray}");
        assert_eq!(code(&commit(c, 3, 1)), Some(251));
        // killSessions takes the session's cursors with it.
        dispatch(&doc! {"killSessions": [lsid(1)]}, c);
        assert_eq!(code(&more(c, mine, doc! {"lsid": lsid(1)})), Some(43));
    });
}

/// `endSessions` leaves an open transaction alone, as mongod does;
/// `killSessions` aborts it.
#[test]
fn ending_a_session_does_not_abort_its_transaction_and_killing_it_does() {
    with_wt(|c| {
        seed(c);
        tx(
            c,
            1,
            1,
            true,
            doc! {"insert": "c", "documents": [{"_id": 3}]},
        );
        dispatch(&doc! {"endSessions": [lsid(1)]}, c);
        assert_eq!(commit(c, 1, 1).get_f64("ok").ok(), Some(1.0));
        tx(
            c,
            1,
            2,
            true,
            doc! {"insert": "c", "documents": [{"_id": 4}]},
        );
        dispatch(&doc! {"killSessions": [lsid(1)]}, c);
        assert_eq!(code(&commit(c, 1, 2)), Some(251));
        assert_eq!(ids(c, "c"), vec![1, 2, 3]);
        assert_eq!(code(&dispatch(&doc! {"endSessions": 1}, c)), Some(14));
    });
}

/// A write sent with `w: 0` runs and is told nothing.
#[test]
fn an_unacknowledged_write_reports_nothing() {
    with_wt(|c| {
        seed(c);
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 3}], "ordered": false,
            "writeConcern": {"w": 0}},
            c,
        );
        assert_eq!(reply.get_i32("n").ok(), Some(0), "{reply}");
        assert!(!reply.contains_key("writeErrors"), "{reply}");
        assert_eq!(ids(c, "c"), vec![1, 2, 3]);
    });
}

/// A retried `bulkWrite` replays the ops that wrote and runs the rest
/// (measured 8.2.11, 2026-10-14). It ran every op again: a `$inc` counted
/// twice, an insert failed as a duplicate.
#[test]
fn a_retried_bulk_write_is_replayed() {
    with_wt(|c| {
        seed(c);
        let ops = || {
            vec![
                Bson::Document(doc! {"insert": 0, "document": {"_id": 10}}),
                Bson::Document(
                    doc! {"update": 0, "filter": {"_id": 1}, "updateMods": {"$inc": {"v": 1}}},
                ),
                Bson::Document(doc! {"delete": 0, "filter": {"_id": 2}}),
            ]
        };
        let ns = format!("{}.c", c.db_name);
        let bulk = |c: &mut CommandContext, ops: Vec<Bson>, number: i64| {
            let mut cmd = doc! {"bulkWrite": 1, "ops": ops, "nsInfo": [{"ns": &ns}]};
            cmd.insert("lsid", lsid(9));
            cmd.insert("txnNumber", number);
            dispatch_admin(&cmd, c)
        };
        let first = bulk(c, ops(), 1);
        assert!(!first.contains_key("retriedStmtIds"), "{first}");
        let again = bulk(c, ops(), 1);
        assert_eq!(
            again.get_array("retriedStmtIds").unwrap(),
            &vec![Bson::Int32(0), Bson::Int32(1), Bson::Int32(2)]
        );
        for (field, n) in [
            ("nInserted", 1),
            ("nMatched", 1),
            ("nModified", 1),
            ("nDeleted", 1),
        ] {
            assert_eq!(again.get_i32(field), Ok(n), "{field}: {again}");
        }
        assert_eq!(
            again
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch"),
            first
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch"),
        );
        assert_eq!(ids(c, "c"), vec![1, 10]);
        assert_eq!(value(c, 1), Some(Bson::Int32(2)));
        // A new op in the retry runs; the old one is replayed.
        let mut more = ops();
        more.push(Bson::Document(doc! {"insert": 0, "document": {"_id": 11}}));
        let partial = bulk(c, more, 1);
        assert_eq!(partial.get_i32("nInserted"), Ok(2), "{partial}");
        assert_eq!(ids(c, "c"), vec![1, 10, 11]);
        // A multi op cannot be retried.
        let multi = bulk(
            c,
            vec![Bson::Document(
                doc! {"update": 0, "filter": {}, "updateMods": {"$inc": {"v": 1}}, "multi": true},
            )],
            2,
        );
        let entry = multi
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(entry.get_i32("code"), Ok(72), "{multi}");
        assert_eq!(value(c, 1), Some(Bson::Int32(2)));
    });
}

fn collections(c: &mut CommandContext) -> Vec<String> {
    let reply = dispatch_full(&doc! {"listCollections": 1, "nameOnly": true}, c);
    let mut names: Vec<String> = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|d| {
            d.as_document()
                .unwrap()
                .get_str("name")
                .unwrap()
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// A collection a transaction creates is its own until it commits: another
/// transaction may create it too, and the second COMMIT fails with 112,
/// whichever began first; a plain insert that creates it meanwhile wins
/// (measured 8.2.11, 2026-10-14). The second transaction failed at its
/// first insert.
#[test]
fn a_collection_created_in_a_transaction_is_registered_at_commit() {
    with_wt(|c| {
        seed(c);
        let in_use = |reply: &Document| {
            assert_eq!(code(reply), Some(112), "{reply}");
            assert_eq!(
                reply.get_array("errorLabels").ok().map(|l| l.len()),
                Some(1),
                "{reply}"
            );
            assert!(
                reply
                    .get_str("errmsg")
                    .unwrap()
                    .contains("Collection namespace '"),
                "{reply}"
            );
        };
        // The later one commits first, and wins.
        let r = tx(
            c,
            1,
            1,
            true,
            doc! {"insert": "n", "documents": [{"_id": 1}]},
        );
        assert_eq!(r.get_f64("ok"), Ok(1.0), "{r}");
        let r = tx(
            c,
            2,
            1,
            true,
            doc! {"insert": "n", "documents": [{"_id": 2}]},
        );
        assert_eq!(r.get_f64("ok"), Ok(1.0), "{r}");
        // Neither is seen outside its transaction.
        assert!(!collections(c).contains(&"n".to_string()));
        assert_eq!(ids(c, "n"), Vec::<i32>::new());
        // Each sees its own.
        let mine = tx(c, 2, 1, false, doc! {"find": "n"});
        assert_eq!(
            mine.get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .len(),
            1
        );
        let cr = commit(c, 2, 1); assert_eq!(cr.get_f64("ok"), Ok(1.0), "{cr}");
        in_use(&commit(c, 1, 1));
        assert_eq!(ids(c, "n"), vec![2]);

        // A plain insert that creates the collection meanwhile wins too.
        let r = tx(
            c,
            3,
            1,
            true,
            doc! {"insert": "m", "documents": [{"_id": 1}]},
        );
        assert_eq!(r.get_f64("ok"), Ok(1.0), "{r}");
        let r = dispatch_full(&doc! {"insert": "m", "documents": [{"_id": 2}]}, c);
        assert_eq!(r.get_i32("n"), Ok(1), "{r}");
        in_use(&commit(c, 3, 1));
        assert_eq!(ids(c, "m"), vec![2]);

        // An abort leaves nothing behind; the other transaction commits.
        tx(
            c,
            4,
            1,
            true,
            doc! {"insert": "k", "documents": [{"_id": 1}]},
        );
        tx(
            c,
            5,
            1,
            true,
            doc! {"insert": "k", "documents": [{"_id": 2}]},
        );
        dispatch_admin(
            &doc! {"abortTransaction": 1, "lsid": lsid(4), "txnNumber": 1_i64, "autocommit": false},
            c,
        );
        assert_eq!(commit(c, 5, 1).get_f64("ok"), Ok(1.0));
        assert_eq!(ids(c, "k"), vec![2]);

        // By name, too.
        tx(c, 6, 1, true, doc! {"create": "byname"});
        tx(c, 7, 1, true, doc! {"create": "byname"});
        assert_eq!(commit(c, 6, 1).get_f64("ok"), Ok(1.0));
        in_use(&commit(c, 7, 1));
        assert!(collections(c).contains(&"byname".to_string()));

        // Both store one `_id`: neither insert conflicts, and the second
        // commit fails. The `_id` index rows are kept under a name of the
        // transaction's own until it commits; it used to be the second insert.
        let r = tx(
            c,
            8,
            1,
            true,
            doc! {"insert": "same", "documents": [{"_id": 1, "v": 8}]},
        );
        assert_eq!(r.get_f64("ok"), Ok(1.0), "{r}");
        let r = tx(
            c,
            9,
            1,
            true,
            doc! {"insert": "same", "documents": [{"_id": 1, "v": 9}]},
        );
        assert_eq!(r.get_f64("ok"), Ok(1.0), "{r}");
        // Inside each, the `_id` reads and writes as usual. (A duplicate
        // would abort the transaction, as any failed write in one does.)
        let r = tx(
            c,
            9,
            1,
            false,
            doc! {"update": "same",
            "updates": [{"q": {"_id": 1}, "u": {"$inc": {"v": 1}}}]},
        );
        assert_eq!(r.get_i32("nModified"), Ok(1), "{r}");
        let r = tx(c, 9, 1, false, doc! {"find": "same", "filter": {"_id": 1}});
        let got = r
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()[0]
            .as_document()
            .unwrap()
            .get_i32("v");
        assert_eq!(got, Ok(10), "{r}");
        assert_eq!(commit(c, 9, 1).get_f64("ok"), Ok(1.0));
        in_use(&commit(c, 8, 1));
        let r = dispatch_full(&doc! {"find": "same"}, c);
        let docs = r
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(docs.len(), 1, "{r}");
        assert_eq!(docs[0].as_document().unwrap().get_i32("v"), Ok(10));
        // The committed `_id` index is the collection's own: a plain insert
        // of the same `_id` is a duplicate, and a delete by `_id` finds it.
        let r = dispatch_full(&doc! {"insert": "same", "documents": [{"_id": 1}]}, c);
        assert!(r.contains_key("writeErrors"), "{r}");
        let r = dispatch_full(
            &doc! {"delete": "same", "deletes": [{"q": {"_id": 1}, "limit": 1}]},
            c,
        );
        assert_eq!(r.get_i32("n"), Ok(1), "{r}");
    });
}
