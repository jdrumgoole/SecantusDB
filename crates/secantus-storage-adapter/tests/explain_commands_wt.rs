//! `explain` over real WiredTiger: the plan each command reports, its index
//! bounds, what running it counts, and what the command refuses. Every
//! expectation is what mongod 8.2.11 answered to the same command
//! (`tools/probes/explain_commands.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

/// The collection the probe uses: one usable index per field.
fn seed(c: &mut CommandContext) {
    let mut docs: Vec<Document> = (1..25)
        .map(|i| {
            doc! {
                "_id": i, "a": i % 5, "b": i % 4, "c": i % 3, "d": i % 7,
                "t": [i, i + 1], "s": format!("s{i:02}"), "p": i, "q": i - 10,
                "n": { "k": i % 6 }, "x": i % 5,
            }
        })
        .collect();
    docs.push(doc! { "_id": 100, "a": Bson::Null, "x": Bson::Null });
    docs.push(doc! { "_id": 101, "a": "str", "b": "str" });
    docs.push(doc! { "_id": 102 });
    let inserted = dispatch_full(&doc! { "insert": "c", "documents": docs }, c);
    assert_eq!(inserted.get_i32("n"), Ok(27), "{inserted}");
    let built = dispatch_full(
        &doc! { "createIndexes": "c", "indexes": [
            { "key": { "a": 1 }, "name": "a_1" },
            { "key": { "b": -1 }, "name": "b_-1" },
            { "key": { "c": 1, "d": 1 }, "name": "c_1_d_1" },
            { "key": { "t": 1 }, "name": "t_1" },
            { "key": { "s": 1 }, "name": "s_1", "unique": true, "sparse": true },
            { "key": { "p": 1 }, "name": "p_1",
              "partialFilterExpression": { "q": { "$gt": 0 } } },
            { "key": { "n.k": 1 }, "name": "n.k_1" },
        ] },
        c,
    );
    assert_eq!(built.get_f64("ok"), Ok(1.0), "{built}");
}

fn explain(c: &mut CommandContext, command: Document, verbosity: &str) -> Document {
    dispatch_full(&doc! { "explain": command, "verbosity": verbosity }, c)
}

/// A plan on one line: `FETCH <- IXSCAN a_1 forward {"a": ["[2, 2]"]}`.
fn text(stage: &Document) -> String {
    let name = stage.get_str("stage").unwrap_or("?");
    let mut out = name.to_string();
    if let Ok(index) = stage.get_str("indexName") {
        out.push_str(&format!(" {index}"));
    }
    if let (Ok(direction), true) = (stage.get_str("direction"), name != "COLLSCAN") {
        out.push_str(&format!(" {direction}"));
    }
    if let Ok(bounds) = stage.get_document("indexBounds") {
        out.push_str(&format!(" {bounds}"));
    }
    for key in ["limitAmount", "skipAmount"] {
        if let Some(n) = stage.get(key) {
            out.push_str(&format!(" {n}"));
        }
    }
    if stage.contains_key("filter") {
        out.push_str(" filtered");
    }
    if let Ok(input) = stage.get_document("inputStage") {
        out.push_str(&format!(" <- {}", text(input)));
    }
    if let Ok(inputs) = stage.get_array("inputStages") {
        let parts: Vec<String> = inputs
            .iter()
            .filter_map(Bson::as_document)
            .map(text)
            .collect();
        out.push_str(&format!(" <- [{}]", parts.join(" | ")));
    }
    out
}

fn winning(reply: &Document) -> String {
    let planner = reply
        .get_document("queryPlanner")
        .unwrap_or_else(|_| panic!("no queryPlanner: {reply}"));
    text(planner.get_document("winningPlan").unwrap())
}

fn find_plan(c: &mut CommandContext, filter: Document) -> String {
    let reply = explain(c, doc! { "find": "c", "filter": filter }, "queryPlanner");
    winning(&reply)
}

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

/// A predicate on an indexed field becomes that index's bounds.
#[test]
fn predicates_become_index_bounds() {
    with_wt(|c| {
        seed(c);
        let cases: Vec<(Document, &str)> = vec![
            (
                doc! { "a": 2 },
                r#"FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#,
            ),
            (
                doc! { "a": { "$gt": 1, "$lt": 4 } },
                r#"FETCH <- IXSCAN a_1 forward { "a": ["(1, 4)"] }"#,
            ),
            (
                doc! { "a": { "$gt": 4, "$lt": 1 } },
                r#"FETCH <- IXSCAN a_1 forward { "a": [] }"#,
            ),
            (
                doc! { "a": { "$in": [3, 1, 3] } },
                r#"FETCH <- IXSCAN a_1 forward { "a": ["[1, 1]", "[3, 3]"] }"#,
            ),
            (
                doc! { "a": { "$ne": 2 } },
                r#"FETCH <- IXSCAN a_1 forward { "a": ["[MinKey, 2)", "(2, MaxKey]"] }"#,
            ),
            (
                doc! { "a": Bson::Null },
                r#"FETCH filtered <- IXSCAN a_1 forward { "a": ["[null, null]"] }"#,
            ),
            (
                doc! { "a": { "$gt": "s" } },
                r#"FETCH <- IXSCAN a_1 forward { "a": ["("s", {})"] }"#,
            ),
            (
                doc! { "a": { "$exists": true } },
                r#"FETCH filtered <- IXSCAN a_1 forward { "a": ["[MinKey, MaxKey]"] }"#,
            ),
            // A descending index prints its bounds from high to low.
            (
                doc! { "b": { "$gt": 2 } },
                r#"FETCH <- IXSCAN b_-1 forward { "b": ["[inf, 2)"] }"#,
            ),
            (
                doc! { "b": { "$in": [1, 3] } },
                r#"FETCH <- IXSCAN b_-1 forward { "b": ["[3, 3]", "[1, 1]"] }"#,
            ),
            (
                doc! { "c": 1 },
                r#"FETCH <- IXSCAN c_1_d_1 forward { "c": ["[1, 1]"], "d": ["[MinKey, MaxKey]"] }"#,
            ),
            (
                doc! { "c": { "$gt": 0 }, "d": 2 },
                r#"FETCH <- IXSCAN c_1_d_1 forward { "c": ["(0, inf]"], "d": ["[2, 2]"] }"#,
            ),
            (
                doc! { "c": 1, "x": 3 },
                r#"FETCH filtered <- IXSCAN c_1_d_1 forward { "c": ["[1, 1]"], "d": ["[MinKey, MaxKey]"] }"#,
            ),
            // Not led by a predicate: the index is no use.
            (doc! { "d": 2 }, "COLLSCAN filtered"),
            // Two predicates on a multikey field may match different elements,
            // so only one bounds the scan; `$elemMatch` binds them to one.
            (
                doc! { "t": { "$gt": 3, "$lt": 9 } },
                r#"FETCH filtered <- IXSCAN t_1 forward { "t": ["[-inf, 9)"] }"#,
            ),
            (
                doc! { "t": { "$elemMatch": { "$gt": 3, "$lt": 9 } } },
                r#"FETCH filtered <- IXSCAN t_1 forward { "t": ["(3, 9)"] }"#,
            ),
            (
                doc! { "s": { "$regex": "^s0" } },
                r#"FETCH <- IXSCAN s_1 forward { "s": ["["s0", "s1")", "[/^s0/, /^s0/]"] }"#,
            ),
            // A sparse index cannot answer for a missing field.
            (doc! { "s": Bson::Null }, "COLLSCAN filtered"),
            (doc! { "s": { "$ne": "s03" } }, "COLLSCAN filtered"),
            // A partial index serves only a query that implies its filter.
            (doc! { "p": 5 }, "COLLSCAN filtered"),
            (
                doc! { "p": 15, "q": { "$gt": 0 } },
                r#"FETCH <- IXSCAN p_1 forward { "p": ["[15, 15]"] }"#,
            ),
            (
                doc! { "n.k": 2 },
                r#"FETCH <- IXSCAN n.k_1 forward { "n.k": ["[2, 2]"] }"#,
            ),
            (doc! { "a": { "$in": [] } }, "EOF"),
        ];
        for (filter, want) in cases {
            assert_eq!(find_plan(c, filter.clone()), want, "{filter}");
        }
    });
}

/// A point lookup through a unique index is its own stage.
#[test]
fn a_unique_point_lookup_is_express() {
    with_wt(|c| {
        seed(c);
        let reply = explain(
            c,
            doc! { "find": "c", "filter": { "_id": 3 } },
            "executionStats",
        );
        let plan = reply
            .get_document("queryPlanner")
            .unwrap()
            .get_document("winningPlan")
            .unwrap();
        assert_eq!(plan.get_str("stage"), Ok("EXPRESS_IXSCAN"));
        assert_eq!(plan.get_str("keyPattern"), Ok("{ _id: 1 }"));
        let stats = reply.get_document("executionStats").unwrap();
        assert_eq!(stats.get_i32("totalKeysExamined"), Ok(1));
        assert_eq!(stats.get_i32("totalDocsExamined"), Ok(1));
        assert_eq!(find_plan(c, doc! { "s": "s03" }), "EXPRESS_IXSCAN s_1");
        // Not a point: a null, a range, or a second predicate.
        assert_eq!(
            find_plan(c, doc! { "_id": { "$gt": 3 } }),
            r#"FETCH <- IXSCAN _id_ forward { "_id": ["(3, inf]"] }"#
        );
        // With a skip the lookup is an ordinary scan.
        let skipped = explain(
            c,
            doc! { "find": "c", "filter": { "_id": 3 }, "skip": 1 },
            "queryPlanner",
        );
        assert_eq!(
            winning(&skipped),
            r#"FETCH <- SKIP 1 <- IXSCAN _id_ forward { "_id": ["[3, 3]"] }"#
        );
    });
}

/// Sort, paging and projection nest around the scan as mongod nests them.
#[test]
fn sort_paging_and_projection_wrap_the_scan() {
    with_wt(|c| {
        seed(c);
        let cases: Vec<(Document, &str)> = vec![
            (
                doc! { "sort": { "a": -1 } },
                r#"FETCH <- IXSCAN a_1 backward { "a": ["[MaxKey, MinKey]"] }"#,
            ),
            (doc! { "sort": { "x": 1 } }, "SORT <- COLLSCAN"),
            (
                doc! { "sort": { "x": 1 }, "limit": 3, "skip": 2 },
                "SKIP 2 <- SORT 5 <- COLLSCAN",
            ),
            (
                doc! { "sort": { "a": 1 }, "limit": 3, "skip": 2 },
                r#"LIMIT 3 <- FETCH <- SKIP 2 <- IXSCAN a_1 forward { "a": ["[MinKey, MaxKey]"] }"#,
            ),
            (
                doc! { "filter": { "a": 2 }, "sort": { "x": 1 } },
                r#"SORT <- FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#,
            ),
            // The sort follows the index once its leading field is pinned.
            (
                doc! { "filter": { "c": 1 }, "sort": { "d": -1 } },
                r#"FETCH <- IXSCAN c_1_d_1 backward { "c": ["[1, 1]"], "d": ["[MaxKey, MinKey]"] }"#,
            ),
            // A few points and a sort after them: one scan per point, merged.
            (
                doc! { "filter": { "c": { "$in": [0, 1] } }, "sort": { "d": 1 } },
                concat!(
                    "FETCH <- SORT_MERGE <- [",
                    r#"IXSCAN c_1_d_1 forward { "c": ["[0, 0]"], "d": ["[MinKey, MaxKey]"] }"#,
                    " | ",
                    r#"IXSCAN c_1_d_1 forward { "c": ["[1, 1]"], "d": ["[MinKey, MaxKey]"] }"#,
                    "]"
                ),
            ),
            (
                doc! { "filter": { "a": 2 }, "projection": { "a": 1, "_id": 0 } },
                r#"PROJECTION_COVERED <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#,
            ),
            (
                doc! { "filter": { "a": 2 }, "projection": { "a": 1 } },
                r#"PROJECTION_SIMPLE <- FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#,
            ),
            (doc! { "hint": { "$natural": -1 } }, "COLLSCAN"),
            (
                doc! { "filter": { "x": 2 }, "hint": "a_1" },
                r#"FETCH filtered <- IXSCAN a_1 forward { "a": ["[MinKey, MaxKey]"] }"#,
            ),
            (
                doc! { "filter": { "$or": [{ "a": 1 }, { "b": 2 }] } },
                concat!(
                    "SUBPLAN <- FETCH <- OR <- [",
                    r#"IXSCAN a_1 forward { "a": ["[1, 1]"] }"#,
                    " | ",
                    r#"IXSCAN b_-1 forward { "b": ["[2, 2]"] }"#,
                    "]"
                ),
            ),
            (
                doc! { "filter": { "$or": [{ "a": 1 }, { "x": 2 }] } },
                "SUBPLAN <- COLLSCAN filtered",
            ),
        ];
        for (options, want) in cases {
            let mut command = doc! { "find": "c" };
            command.extend(options.clone());
            let reply = explain(c, command, "queryPlanner");
            assert_eq!(winning(&reply), want, "{options}");
        }
    });
}

/// `count` and `distinct` have stages of their own.
#[test]
fn count_and_distinct_have_their_own_stages() {
    with_wt(|c| {
        seed(c);
        let plan = |c: &mut CommandContext, command: Document| {
            winning(&explain(c, command, "queryPlanner"))
        };
        assert_eq!(plan(c, doc! { "count": "c" }), "RECORD_STORE_FAST_COUNT");
        assert_eq!(
            plan(c, doc! { "count": "c", "query": { "a": 2 } }),
            r#"COUNT <- COUNT_SCAN a_1 { "startKey": { "a": 2 }, "startKeyInclusive": true, "endKey": { "a": 2 }, "endKeyInclusive": true }"#
        );
        assert_eq!(
            plan(
                c,
                doc! { "count": "c", "query": { "a": { "$in": [1, 3] } } }
            ),
            r#"COUNT <- IXSCAN a_1 forward { "a": ["[1, 1]", "[3, 3]"] }"#
        );
        assert_eq!(
            plan(c, doc! { "count": "c", "query": { "_id": 3 } }),
            "COUNT <- IDHACK"
        );
        assert_eq!(
            plan(c, doc! { "count": "c", "query": { "x": 2 } }),
            "COUNT <- COLLSCAN filtered"
        );
        assert_eq!(plan(c, doc! { "count": "nope" }), "COUNT <- EOF");
        assert_eq!(
            plan(c, doc! { "distinct": "c", "key": "a" }),
            r#"PROJECTION_COVERED <- DISTINCT_SCAN a_1 forward { "a": ["[MinKey, MaxKey]"] }"#
        );
        assert_eq!(
            plan(c, doc! { "distinct": "c", "key": "d", "query": { "c": 1 } }),
            r#"PROJECTION_COVERED <- DISTINCT_SCAN c_1_d_1 forward { "c": ["[1, 1]"], "d": ["[MinKey, MaxKey]"] }"#
        );
        assert_eq!(plan(c, doc! { "distinct": "c", "key": "x" }), "COLLSCAN");
        assert_eq!(
            plan(c, doc! { "distinct": "c", "key": "x", "query": { "a": 2 } }),
            r#"FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#
        );
        // What the count examined: the keys in range, and the one past them.
        let counted = explain(
            c,
            doc! { "count": "c", "query": { "a": 2 } },
            "executionStats",
        );
        let stats = counted.get_document("executionStats").unwrap();
        assert_eq!(stats.get_i32("totalKeysExamined"), Ok(6));
        assert_eq!(stats.get_i32("totalDocsExamined"), Ok(0));
        let stages = stats.get_document("executionStages").unwrap();
        assert_eq!(stages.get_i32("nCounted"), Ok(5));
        // `count` comes back parsed: a query, 64-bit paging, `$hint`.
        let echoed = explain(
            c,
            doc! { "count": "c", "skip": 2, "hint": "a_1", "limit": -3 },
            "queryPlanner",
        );
        assert_eq!(
            echoed.get_document("command").unwrap(),
            &doc! { "count": "c", "query": {}, "limit": 3_i64, "skip": 2_i64,
            "hint": { "$hint": "a_1" } }
        );
    });
}

/// An explained write reports what it would do and changes nothing.
#[test]
fn an_explained_write_changes_nothing() {
    with_wt(|c| {
        seed(c);
        let update = |q: Document, multi: bool| {
            doc! { "update": "c", "updates": [{ "q": q, "u": { "$set": { "z": 1 } }, "multi": multi }] }
        };
        let reply = explain(c, update(doc! { "a": 2 }, true), "executionStats");
        assert_eq!(
            winning(&reply),
            r#"UPDATE <- FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#
        );
        let stages = reply
            .get_document("executionStats")
            .unwrap()
            .get_document("executionStages")
            .unwrap();
        assert_eq!(stages.get_i32("nMatched"), Ok(5));
        assert_eq!(stages.get_i32("nWouldModify"), Ok(5));
        assert_eq!(stages.get_i32("nWouldUpsert"), Ok(0));
        let one = explain(c, update(doc! { "_id": 3 }, false), "queryPlanner");
        assert_eq!(winning(&one), "EXPRESS_UPDATE _id_");
        let delete =
            |limit: i32| doc! { "delete": "c", "deletes": [{ "q": { "a": 2 }, "limit": limit }] };
        let many = explain(c, delete(0), "executionStats");
        assert!(
            winning(&many).starts_with("BATCHED_DELETE <- FETCH"),
            "{many}"
        );
        let would = many
            .get_document("executionStats")
            .unwrap()
            .get_document("executionStages")
            .unwrap();
        assert_eq!(would.get_i32("nWouldDelete"), Ok(5));
        assert!(winning(&explain(c, delete(1), "queryPlanner")).starts_with("DELETE <- FETCH"));
        let sorted = explain(
            c,
            doc! { "findAndModify": "c", "query": { "a": 2 }, "remove": true, "sort": { "x": 1 } },
            "queryPlanner",
        );
        assert_eq!(
            winning(&sorted),
            r#"DELETE <- SORT 1 <- FETCH <- IXSCAN a_1 forward { "a": ["[2, 2]"] }"#
        );
        // Nothing was written.
        let after = dispatch_full(&doc! { "count": "c" }, c);
        assert_eq!(after.get_i32("n"), Ok(27));
        let touched = dispatch_full(&doc! { "count": "c", "query": { "z": 1 } }, c);
        assert_eq!(touched.get_i32("n"), Ok(0));
        // One statement at a time.
        let two = explain(
            c,
            doc! { "delete": "c", "deletes": [{ "q": {}, "limit": 0 }, { "q": {}, "limit": 0 }] },
            "queryPlanner",
        );
        assert_eq!(
            refusal(&two),
            (16, "explained write batches must be of size 1".to_string())
        );
    });
}

/// A stage stops its input as soon as it has what it needs.
#[test]
fn execution_counts_follow_the_plan() {
    with_wt(|c| {
        seed(c);
        let counts = |c: &mut CommandContext, command: Document| {
            let reply = explain(c, command, "executionStats");
            let stats = reply.get_document("executionStats").unwrap().clone();
            (
                stats.get_i32("nReturned").unwrap(),
                stats.get_i32("totalKeysExamined").unwrap(),
                stats.get_i32("totalDocsExamined").unwrap(),
            )
        };
        let find = |filter: Document| doc! { "find": "c", "filter": filter };
        assert_eq!(counts(c, find(doc! {})), (27, 0, 27));
        assert_eq!(counts(c, find(doc! { "a": 2 })), (5, 5, 5));
        // The scan examines the key that ends each interval.
        assert_eq!(
            counts(c, find(doc! { "a": { "$in": [1, 3] } })),
            (10, 12, 10)
        );
        assert_eq!(counts(c, find(doc! { "a": 2, "x": 3 })), (0, 5, 5));
        // A multikey scan meets a document once per matching element.
        assert_eq!(counts(c, find(doc! { "t": { "$in": [3, 4] } })), (3, 5, 3));
        assert_eq!(counts(c, doc! { "find": "c", "limit": 3 }), (3, 0, 3));
        assert_eq!(
            counts(c, doc! { "find": "c", "filter": { "x": 2 }, "limit": 2 }),
            (2, 0, 7)
        );
        assert_eq!(
            counts(c, doc! { "find": "c", "sort": { "x": 1 }, "limit": 3 }),
            (3, 0, 27)
        );
        assert_eq!(
            counts(
                c,
                doc! { "find": "c", "filter": { "a": 2 }, "projection": { "a": 1, "_id": 0 } }
            ),
            (5, 5, 0)
        );
        assert_eq!(
            counts(c, find(doc! { "a": 2 }).tap("find", "nope")),
            (0, 0, 0)
        );
    });
}

trait Tap {
    fn tap(self, key: &str, value: &str) -> Self;
}

impl Tap for Document {
    fn tap(mut self, key: &str, value: &str) -> Self {
        self.insert(key, value);
        self
    }
}

/// What `explain` refuses, in mongod's words.
#[test]
fn explain_checks_its_arguments() {
    with_wt(|c| {
        seed(c);
        let find = doc! { "find": "c", "filter": { "a": 2 } };
        let sent = |c: &mut CommandContext, command: Document| refusal(&dispatch_full(&command, c));
        assert_eq!(
            sent(c, doc! { "explain": find.clone(), "verbosity": "nope" }),
            (
                2,
                "Enumeration value 'nope' for field 'explain.verbosity' is not a valid value."
                    .to_string()
            )
        );
        assert_eq!(
            sent(c, doc! { "explain": find.clone(), "verbosity": 1 }).0,
            14
        );
        assert_eq!(
            sent(c, doc! { "explain": 1 }),
            (
                14,
                "BSON field 'explain.explain' is the wrong type 'int', expected type 'object'"
                    .to_string()
            )
        );
        assert_eq!(
            sent(c, doc! { "explain": find.clone(), "nope": 1 }),
            (
                40415,
                "BSON field 'explain.nope' is an unknown field.".to_string()
            )
        );
        assert_eq!(
            sent(
                c,
                doc! { "explain": find.clone(), "writeConcern": { "w": 1 } }
            ),
            (72, "Command does not support writeConcern".to_string())
        );
        // An explained aggregate takes no write concern, in either form.
        let refused = "Aggregation explain does not support the'writeConcern' option";
        let wrapped = doc! { "explain": {
            "aggregate": "c", "pipeline": [], "cursor": {}, "writeConcern": { "w": 1 },
        } };
        assert_eq!(sent(c, wrapped), (9, refused.to_string()));
        let inline = doc! {
            "aggregate": "c", "pipeline": [], "explain": true, "writeConcern": { "j": true },
        };
        assert_eq!(sent(c, inline), (9, refused.to_string()));
        assert_eq!(
            sent(c, doc! { "explain": { "nope": "c" } }),
            (
                59,
                "Explain failed due to unknown command: nope".to_string()
            )
        );
        assert_eq!(
            sent(
                c,
                doc! { "explain": { "insert": "c", "documents": [{ "_id": 500 }] } }
            ),
            (20, "Cannot explain cmd: insert".to_string())
        );
        assert_eq!(
            sent(c, doc! { "explain": { "explain": { "find": "c" } } }),
            (20, "Explain cannot explain itself.".to_string())
        );
        assert_eq!(
            sent(
                c,
                doc! { "explain": { "find": "c", "filter": {}, "$db": "other" } }
            )
            .0,
            73
        );
        // No verbosity is the fullest one.
        let full = dispatch_full(&doc! { "explain": find }, c);
        let stats = full.get_document("executionStats").unwrap();
        assert!(stats.contains_key("allPlansExecution"), "{full}");
        assert_eq!(full.get_str("explainVersion"), Ok("1"));
    });
}

/// An operator the query language does not have is refused when the filter is
/// parsed, whether or not the collection has a document to test it on. Over
/// an empty or missing collection these commands used to answer as if the
/// filter were valid.
#[test]
fn an_unknown_operator_is_refused_without_a_document() {
    with_wt(|c| {
        dispatch_full(&doc! { "create": "e" }, c);
        let bad = doc! { "a": { "$nope": 1 } };
        for coll in ["e", "missing"] {
            let reads = [
                doc! { "find": coll, "filter": bad.clone() },
                doc! { "count": coll, "query": bad.clone() },
                doc! { "distinct": coll, "key": "a", "query": bad.clone() },
                doc! { "aggregate": coll, "pipeline": [{ "$match": bad.clone() }], "cursor": {} },
                doc! { "findAndModify": coll, "query": bad.clone(), "remove": true },
            ];
            for command in reads {
                let reply = dispatch_full(&command, c);
                assert_eq!(
                    refusal(&reply),
                    (2, "unknown operator: $nope".to_string()),
                    "{command}"
                );
            }
            let writes = [
                doc! { "update": coll, "updates": [{ "q": bad.clone(), "u": { "$set": { "z": 1 } } }] },
                doc! { "delete": coll, "deletes": [{ "q": bad.clone(), "limit": 0 }] },
            ];
            for command in writes {
                let reply = dispatch_full(&command, c);
                let errors = reply
                    .get_array("writeErrors")
                    .unwrap_or_else(|_| panic!("no writeErrors for {command}: {reply}"));
                let first = errors[0].as_document().unwrap();
                assert_eq!(first.get_str("errmsg"), Ok("unknown operator: $nope"));
            }
        }
        let top = dispatch_full(&doc! { "count": "e", "query": { "$nope": [1] } }, c);
        assert!(refusal(&top)
            .1
            .starts_with("unknown top level operator: $nope"));
        // A valid filter over nothing is still nothing.
        let fine = dispatch_full(&doc! { "count": "e", "query": { "a": { "$gt": 1 } } }, c);
        assert_eq!(fine.get_i32("n"), Ok(0));
    });
}

/// A read on a view is planned as the view's pipeline on what it reads.
#[test]
fn a_view_is_explained_through_its_pipeline() {
    with_wt(|c| {
        seed(c);
        let made = dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [{ "$match": { "a": { "$gt": 1 } } }] },
            c,
        );
        assert_eq!(made.get_f64("ok"), Ok(1.0), "{made}");
        let reply = explain(c, doc! { "find": "v", "filter": {} }, "executionStats");
        let planner = reply.get_document("queryPlanner").unwrap();
        assert!(planner.get_str("namespace").unwrap().ends_with(".c"));
        assert_eq!(planner.get_bool("optimizedPipeline"), Ok(true));
        assert_eq!(
            winning(&reply),
            r#"FETCH <- IXSCAN a_1 forward { "a": ["(1, inf]"] }"#
        );
        let stats = reply.get_document("executionStats").unwrap();
        assert_eq!(stats.get_i32("nReturned"), Ok(15));
        let echoed = reply.get_document("command").unwrap();
        assert_eq!(echoed.get_str("aggregate"), Ok("c"));
        assert_eq!(echoed.get_array("pipeline").unwrap().len(), 1);
        let write = explain(
            c,
            doc! { "delete": "v", "deletes": [{ "q": {}, "limit": 0 }] },
            "queryPlanner",
        );
        assert_eq!(refusal(&write).0, 166);
    });
}

/// A lookup by `_id` in a collection that was never written finds nothing.
/// Document tables are created on first write, and the lookup opened the
/// table without allowing for its absence: `count` and `delete` answered an
/// internal WiredTiger error where mongod answers zero.
#[test]
fn a_lookup_by_id_in_a_missing_collection_finds_nothing() {
    with_wt(|c| {
        for coll in ["never", "written", "anywhere", "at", "all"] {
            let counted = dispatch_full(&doc! { "count": coll, "query": { "_id": 3 } }, c);
            assert_eq!(counted.get_i32("n"), Ok(0), "{counted}");
            let deleted = dispatch_full(
                &doc! { "delete": coll, "deletes": [{ "q": { "_id": 3 }, "limit": 1 }] },
                c,
            );
            assert_eq!(deleted.get_i32("n"), Ok(0), "{deleted}");
            assert!(!deleted.contains_key("writeErrors"), "{deleted}");
            let updated = dispatch_full(
                &doc! { "update": coll, "updates": [{ "q": { "_id": 3 }, "u": { "$set": { "a": 1 } } }] },
                c,
            );
            assert_eq!(updated.get_i32("n"), Ok(0), "{updated}");
        }
    });
}
