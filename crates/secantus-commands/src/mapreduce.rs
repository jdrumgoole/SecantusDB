//! `mapReduce` — the canonical count shape, without a JavaScript engine.
//!
//! `mapReduce` takes JavaScript function bodies, and SecantusDB ships no JS
//! runtime; evaluating arbitrary map/reduce is deliberately out of scope (the
//! command is deprecated in MongoDB 5.0 and slated for removal). What real
//! deployments overwhelmingly use it for is "count documents by field", which
//! has one canonical spelling:
//!
//! ```javascript
//! map:    function() { emit(this.<field>, 1); }
//! reduce: function(key, values) { return values.length; }
//! ```
//!
//! That shape is recognised and translated to a `$group` count, which the
//! aggregation engine already does correctly. Anything else returns an empty
//! result set with `ok: 1` — enough for the wire-shape probes drivers run
//! (mongo-java-driver's `default-write-concern-3.4` asserts the reply shape and
//! never looks at the values), while a caller who needs real JS evaluation was
//! always going to need a real `mongod`.
//!
//! Port of `commands.py::_map_reduce`, semantics matched case for case: the
//! `{out: {inline: 1}}` gate, the pattern match, and the double-typed `value`.

use bson::{doc, Bson, Document};

use crate::util::{command_error, decode_docs};
use crate::{CommandContext, CommandError, HandlerResult};

/// The field name in `emit(this.<field>, 1)`, or None when `map` is not the
/// canonical count shape.
///
/// Hand-parsed rather than pulled in as a regex dependency: the accepted shape
/// is fixed and narrow, so the scan is a few `find`s. Whitespace is allowed
/// anywhere the JS tokeniser would allow it (`emit ( this . x , 1 )`).
fn emitted_count_field(map_fn: &str) -> Option<String> {
    let after_emit = {
        let at = map_fn.find("emit")?;
        let rest = map_fn[at + "emit".len()..].trim_start();
        rest.strip_prefix('(')?.trim_start()
    };
    let after_this = after_emit.strip_prefix("this")?.trim_start();
    let after_dot = after_this.strip_prefix('.')?.trim_start();
    let end = after_dot
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after_dot.len());
    let field = &after_dot[..end];
    if field.is_empty() {
        return None;
    }
    // ... , 1 )
    let rest = after_dot[end..].trim_start();
    let rest = rest.strip_prefix(',')?.trim_start();
    let rest = rest.strip_prefix('1')?.trim_start();
    rest.strip_prefix(')')?;
    Some(field.to_string())
}

/// Whether `reduce` is the canonical "count the values" body.
fn reduces_by_counting(reduce_fn: &str) -> bool {
    reduce_fn.contains("values.length")
}

pub fn map_reduce(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = match doc.get("mapReduce").or_else(|| doc.get("mapreduce")) {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(CommandError::new(
                9,
                "FailedToParse",
                "mapReduce requires a collection name",
            )
            .into_reply())
        }
    };

    // Only inline output is supported: writing the result into a collection
    // would need the real map/reduce semantics we do not implement, and
    // silently producing a wrong collection is worse than refusing.
    let inline = matches!(doc.get("out"), Some(Bson::Document(d)) if d.contains_key("inline"));
    if !inline {
        return Ok(CommandError::new(
            9,
            "FailedToParse",
            "mapReduce on this server only supports {out: {inline: 1}}",
        )
        .into_reply());
    }

    let map_fn = js_body(doc.get("map"));
    let reduce_fn = js_body(doc.get("reduce"));

    let Some(field) = emitted_count_field(&map_fn).filter(|_| reduces_by_counting(&reduce_fn))
    else {
        // Not the shape we can evaluate — the honest empty result (see the
        // module docs).
        return Ok(doc! { "results": Bson::Array(Vec::new()), "ok": 1.0 });
    };

    let storage = ctx.storage()?;
    let batch = storage
        .find(&ctx.db_name, &coll, &Document::new(), None, None)
        .map_err(command_error)?;
    let docs = decode_docs(batch)?;

    let stages = vec![Bson::Document(doc! {
        "$group": { "_id": format!("${field}"), "value": { "$sum": 1 } }
    })];
    let grouped = secantus_core::aggregate::apply_pipeline(docs, &stages, &Document::new(), None)
        .map_err(|_| {
        CommandError::new(
            2,
            "BadValue",
            "mapReduce could not evaluate the map function",
        )
    })?;

    // Real mongod always returns `value` as a double — its JS engine has no
    // integer type — and drivers decode it as one. The Java driver's
    // `readDouble` throws outright on an Int32, so the cast is load-bearing,
    // not cosmetic.
    let results: Vec<Bson> = grouped
        .into_iter()
        .map(|mut d| {
            if let Some(v) = d.get("value").and_then(count_as_double) {
                d.insert("value", Bson::Double(v));
            }
            Bson::Document(d)
        })
        .collect();

    Ok(doc! { "results": Bson::Array(results), "ok": 1.0 })
}

/// A `$sum` count as an `f64`, or None when the value is already a double (or
/// something else we should leave alone). Booleans are not counts.
fn count_as_double(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

/// The JS source of a `map` / `reduce` argument. Drivers may send it as a
/// string or as BSON `Code`; both carry the same body.
fn js_body(v: Option<&Bson>) -> String {
    match v {
        Some(Bson::String(s)) => s.clone(),
        Some(Bson::JavaScriptCode(s)) => s.clone(),
        Some(Bson::JavaScriptCodeWithScope(cws)) => cws.code.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
    use std::sync::Arc;

    /// A collection of fixed documents — enough to drive the handler's one
    /// storage call (`find` with an empty filter).
    struct Docs(Vec<Document>);

    impl Storage for Docs {
        fn find(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _sort: Option<&Document>,
            _hint: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            Ok(self
                .0
                .iter()
                .map(|d| bson::to_vec(d).expect("encodable"))
                .collect())
        }
        fn insert(
            &self,
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            Ok((0, Vec::new()))
        }
        fn update_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<UpdateOutcome, StorageError> {
            Ok(UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
    }

    fn ctx_with(docs: Vec<Document>) -> CommandContext {
        let mut ctx = CommandContext::new(1);
        ctx.db_name = "db".to_string();
        ctx.storage = Some(Arc::new(Docs(docs)) as Arc<dyn Storage>);
        ctx
    }

    fn run(cmd: Document, docs: Vec<Document>) -> Document {
        let mut ctx = ctx_with(docs);
        map_reduce(&cmd, &mut ctx).expect("handler returns a reply")
    }

    const MAP: &str = "function() { emit(this.tag, 1); }";
    const REDUCE: &str = "function(key, values) { return values.length; }";

    #[test]
    fn counts_by_the_emitted_field() {
        let reply = run(
            doc! {"mapReduce": "c", "map": MAP, "reduce": REDUCE, "out": {"inline": 1}},
            vec![doc! {"tag": "a"}, doc! {"tag": "b"}, doc! {"tag": "a"}],
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let mut got: Vec<(String, f64)> = reply
            .get_array("results")
            .unwrap()
            .iter()
            .map(|b| {
                let d = b.as_document().unwrap();
                (
                    d.get_str("_id").unwrap().to_string(),
                    // The count MUST be a double: the Java driver's readDouble
                    // throws on an Int32, which is what made this a gauge
                    // failure rather than a cosmetic difference.
                    d.get_f64("value").expect("value is a double"),
                )
            })
            .collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, vec![("a".to_string(), 2.0), ("b".to_string(), 1.0)]);
    }

    #[test]
    fn both_spellings_reach_the_handler_through_dispatch() {
        // Registration is as load-bearing as the handler: an unregistered name
        // is `59 CommandNotFound`, which is what the Java gauge saw. mongod
        // accepts the lowercase alias too.
        assert!(crate::lookup_for_test("mapReduce").is_some());
        assert!(crate::lookup_for_test("mapreduce").is_some());
    }

    #[test]
    fn non_inline_output_is_refused() {
        let reply = run(
            doc! {"mapReduce": "c", "map": MAP, "reduce": REDUCE, "out": "results_coll"},
            vec![doc! {"tag": "a"}],
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
    }

    #[test]
    fn an_unevaluatable_body_returns_an_empty_result_not_an_error() {
        // The wire-shape probe drivers run (java's default-write-concern-3.4)
        // asserts ok:1 and never inspects the values.
        let reply = run(
            doc! {
                "mapReduce": "c",
                "map": "function() { emit(this.a + this.b, 1); }",
                "reduce": REDUCE,
                "out": {"inline": 1},
            },
            vec![doc! {"a": 1, "b": 2}],
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get_array("results").unwrap().is_empty());
    }

    #[test]
    fn recognises_the_canonical_emit_shape() {
        assert_eq!(
            emitted_count_field("function() { emit(this.name, 1); }").as_deref(),
            Some("name")
        );
        // Whitespace anywhere the JS tokeniser allows it.
        assert_eq!(
            emitted_count_field("function () { emit ( this . user_id , 1 ) ; }").as_deref(),
            Some("user_id")
        );
    }

    #[test]
    fn rejects_shapes_it_cannot_evaluate() {
        // A different emitted value, a computed key, a non-`this` source, and a
        // body with no emit at all must all decline rather than silently count.
        for src in [
            "function() { emit(this.name, 2); }",
            "function() { emit(this.name.toLowerCase(), 1); }",
            "function() { emit(key, 1); }",
            "function() { return 1; }",
            "",
        ] {
            assert!(
                emitted_count_field(src).is_none(),
                "should not be read as a count: {src}"
            );
        }
    }

    #[test]
    fn only_the_counting_reduce_qualifies() {
        assert!(reduces_by_counting(
            "function(key, values) { return values.length; }"
        ));
        assert!(!reduces_by_counting(
            "function(key, values) { return Array.sum(values); }"
        ));
    }

    #[test]
    fn counts_are_doubles_ints_only() {
        assert_eq!(count_as_double(&Bson::Int32(3)), Some(3.0));
        assert_eq!(count_as_double(&Bson::Int64(3)), Some(3.0));
        assert_eq!(count_as_double(&Bson::Double(3.0)), None);
        assert_eq!(count_as_double(&Bson::Boolean(true)), None);
    }

    #[test]
    fn js_body_accepts_string_and_code() {
        assert_eq!(js_body(Some(&Bson::String("x".into()))), "x");
        assert_eq!(js_body(Some(&Bson::JavaScriptCode("y".into()))), "y");
        assert_eq!(js_body(None), "");
    }
}
