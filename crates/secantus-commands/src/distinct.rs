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

use crate::util::{coll_arg, collation_of, command_error, doc_field};
use crate::{CommandContext, CommandError, HandlerResult};

/// `distinct` — return the distinct values of `key` over docs matching `query`.
pub fn distinct(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "distinct")?;
    let key = match doc.get("key") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(
                CommandError::new(14, "TypeMismatch", "distinct key must be a string").into_reply(),
            )
        }
    };
    let filter = doc_field(doc, "query");
    let collation = collation_of(doc);
    let storage = ctx.storage()?;
    let bytes = storage
        .find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            None,
            None,
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
