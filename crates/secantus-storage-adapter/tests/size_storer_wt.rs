//! `count` with no query, `collStats`, `$collStats`, `dbStats` and an
//! estimated `dataSize` answer from the size storer, as mongod answers them
//! from its own: without reading a document (RM-191). Over real WiredTiger.

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn ok(reply: Document) -> Document {
    assert!(
        reply.get_f64("ok").is_ok_and(|v| v == 1.0) || reply.get_i32("ok") == Ok(1),
        "{reply}"
    );
    reply
}

fn num(d: &Document, k: &str) -> i64 {
    match d.get(k) {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Double(n)) => *n as i64,
        other => panic!("{k}: {other:?} in {d}"),
    }
}

fn seed(c: &mut CommandContext, coll: &str, n: i32) {
    ok(dispatch_full(
        &doc! {"createIndexes": coll, "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
        c,
    ));
    for start in (0..n).step_by(1000) {
        let docs: Vec<Bson> = (start..(start + 1000).min(n))
            .map(|i| Bson::Document(doc! {"_id": i, "a": i % 13, "p": "x".repeat(20)}))
            .collect();
        ok(dispatch_full(&doc! {"insert": coll, "documents": docs}, c));
    }
}

/// Each command's answer, and how many collection walks it took.
fn answers(c: &mut CommandContext, coll: &str) -> Vec<(&'static str, i64, u64)> {
    let mut out = Vec::new();
    let mut run = |name: &'static str, cmd: Document, pick: &dyn Fn(&Document) -> i64| {
        let walks = secantus_storage::collection_walks();
        let reply = ok(dispatch_full(&cmd, c));
        out.push((
            name,
            pick(&reply),
            secantus_storage::collection_walks() - walks,
        ));
    };
    run("count", doc! {"count": coll}, &|r| num(r, "n"));
    run("collStats", doc! {"collStats": coll}, &|r| {
        num(r, "count") + num(r, "size") + num(r, "totalIndexSize")
    });
    run(
        "$collStats",
        doc! {"aggregate": coll, "pipeline": [{"$collStats": {"storageStats": {}, "count": {}}}], "cursor": {}},
        &|r| {
            let first = r
                .get_document("cursor")
                .and_then(|c| c.get_array("firstBatch"))
                .ok()
                .and_then(|b| b.first())
                .and_then(Bson::as_document)
                .cloned()
                .unwrap_or_else(|| panic!("{r}"));
            num(&first, "count") + num(first.get_document("storageStats").unwrap(), "size")
        },
    );
    run("dbStats", doc! {"dbStats": 1}, &|r| {
        num(r, "objects") + num(r, "dataSize") + num(r, "indexSize")
    });
    run(
        "dataSize estimate",
        doc! {"dataSize": format!("t.{coll}"), "estimate": true},
        &|r| num(r, "numObjects") + num(r, "size"),
    );
    out
}

#[test]
fn the_size_commands_read_no_document() {
    with_wt(|c| {
        seed(c, "small", 200);
        seed(c, "big", 20_000);
        for coll in ["small", "big"] {
            for (name, _, walks) in answers(c, coll) {
                assert_eq!(walks, 0, "{name} on {coll} walked the collection");
            }
        }
        // What they answer is what a walk counts.
        let walks = secantus_storage::collection_walks();
        let exact = ok(dispatch_full(&doc! {"dataSize": "t.big"}, c));
        // A dataSize that is not an estimate reads the collection, as
        // mongod's does.
        assert!(secantus_storage::collection_walks() > walks);
        let count = ok(dispatch_full(&doc! {"count": "big"}, c));
        let stats = ok(dispatch_full(&doc! {"collStats": "big"}, c));
        assert_eq!(num(&count, "n"), 20_000);
        assert_eq!(num(&exact, "numObjects"), 20_000);
        assert_eq!(num(&stats, "size"), num(&exact, "size"));
    });
}

/// mongod's `dataSize` answers zeros, with no `estimate`, for a collection
/// whose record count is 0 -- or that does not exist -- before it looks for
/// an index for the bounds (measured 8.2.11).
#[test]
fn data_size_of_an_empty_collection_is_zeros_without_estimate() {
    with_wt(|c| {
        ok(dispatch_full(&doc! {"create": "empty"}, c));
        for ns in ["t.empty", "t.missing"] {
            for cmd in [
                doc! {"dataSize": ns},
                doc! {"dataSize": ns, "estimate": true},
                doc! {"dataSize": ns, "keyPattern": {"zz": 1}, "min": {"zz": 1}, "max": {"zz": 4}},
            ] {
                let reply = ok(dispatch_full(&cmd, c));
                assert_eq!(
                    reply.keys().map(String::as_str).collect::<Vec<_>>(),
                    ["size", "numObjects", "millis", "ok"],
                    "{cmd}: {reply}"
                );
                assert_eq!((num(&reply, "size"), num(&reply, "numObjects")), (0, 0));
            }
        }
    });
}
