//! The `distinct` command — distinct values of a field across matching docs.
//!
//! Port of `commands.py::_distinct`: fetch the matching documents, resolve the
//! dotted `key` path on each (flattening one array level, as mongod does), and
//! collect the distinct values. A command `collation` applies to both the query
//! filter (collation-aware COLLSCAN) and the value dedup (collation-equal strings
//! collapse to one).

use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use secantus_core::get_path;

use crate::argtypes;
use crate::util::{coll_arg, collation_of, command_error, doc_field};
use crate::{CommandContext, CommandError, HandlerResult};

/// `distinct` — return the distinct values of `key` over docs matching `query`.
pub fn distinct(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "distinct")?;
    // mongod reports these two under the IDL struct name
    // (`distinctCommandRequest`), not under the command name `distinct` that
    // `distinct.key` above uses. Two naming conventions on one command; probed.
    argtypes::require_object(doc, "query", "distinctCommandRequest.query")?;
    argtypes::require_object(doc, "collation", "distinctCommandRequest.collation")?;
    // An undefined `$$variable` is a PARSE error (17276), not the storage
    // layer's generic "unsupported construct" BadValue.
    {
        let bound: Vec<String> = match doc.get("let") {
            Some(Bson::Document(d)) => d.keys().cloned().collect(),
            _ => Vec::new(),
        };
        if let Some(Bson::Document(f)) = doc.get("query") {
            if let Some((code, msg)) = argtypes::expression_problem_in_filter(f, &bound) {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    msg,
                ));
            }
        }
    }

    let key = match doc.get("key") {
        Some(Bson::String(s)) => s.clone(),
        // An explicit null is MISSING to mongod, not wrong-typed: a required
        // field that is absent answers 40414, not a TypeMismatch. Probed.
        None | Some(Bson::Null) => {
            return Ok(CommandError::new(
                40414,
                "Location40414",
                "BSON field 'distinct.key' is missing but a required field",
            )
            .into_reply())
        }
        Some(v) => {
            return Ok(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'distinct.key' is the wrong type '{}', expected type 'string'",
                    secantus_core::query::bson_type_name(v)
                ),
            )
            .into_reply())
        }
    };
    let filter = doc_field(doc, "query");
    let collation = collation_of(doc);
    // `distinct` takes a hint like every other read, and mongod REFUSES the
    // command when it names no index rather than scanning (probed 8.2.11,
    // 2026-08-31: code 2; a valid index name or key spec is accepted). This
    // handler passed `None` here, so a bogus hint silently returned full
    // results -- the only hint-bearing command that did not resolve the field.
    // Passing it validates AND honours the hint, including a sparse index's
    // reduced document set, the way `find` does.
    argtypes::require_hint(doc, "hint")?;
    let hint = doc.get("hint");
    let storage = ctx.storage()?;
    let bytes = storage
        .find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            None,
            hint,
            collation.as_ref(),
            &Document::new(),
        )
        .map_err(command_error)?;

    let mut values: Vec<Bson> = Vec::new();
    for b in bytes {
        let d = Document::from_reader(&mut b.as_slice())
            .map_err(|e| CommandError::new(1, "InternalError", format!("decode: {e}")))?;
        match get_path(&d, &key) {
            // An array value contributes each of its elements (one level).
            Some(Bson::Array(arr)) => {
                for e in arr {
                    push_distinct(&mut values, e, collation.as_ref());
                }
            }
            Some(v) => push_distinct(&mut values, v, collation.as_ref()),
            None => {}
        }
    }
    Ok(doc! { "values": values, "ok": 1.0 })
}

/// Add `v` if no existing value is equal — collation-aware for strings (two
/// collation-equal strings collapse to one). Falls back to BSON equality when
/// no collation, or when the collation can't compare the pair (non-ASCII).
fn push_distinct(values: &mut Vec<Bson>, v: &Bson, collation: Option<&Collation>) {
    let dup = values.iter().any(|e| match (collation, e, v) {
        (Some(c), Bson::String(a), Bson::String(b)) => {
            secantus_core::collation::equal(a, b, c).unwrap_or(a == b)
        }
        _ => e == v,
    });
    if !dup {
        values.push(v.clone());
    }
}
