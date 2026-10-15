//! `RETURNING *` and `RETURNING t.*` on an UPDATE or DELETE, spelled out as
//! the columns they stand for before the statement is planned.
//!
//! PostgreSQL expands a star in RETURNING over every relation the statement
//! reads: the target first, then each FROM / USING item in order, so
//! `UPDATE t ... FROM s ... RETURNING *` returns t's columns followed by s's,
//! and `RETURNING s.*` only s's (measured on 15.19). The planner here takes a
//! bare `*` as the target's columns and refused `t.*` outright, so the FROM
//! columns went missing without an error. Each column becomes a qualified
//! reference named as the column, which the existing paths already plan --
//! a FROM item's through `wrap_foreign_returning`.

use super::*;

fn column(qualifier: &str, name: &str) -> pg_query::protobuf::Node {
    let part = |s: &str| pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String {
            sval: s.to_string(),
        })),
    };
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: name.to_string(),
            val: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                    fields: vec![part(qualifier), part(name)],
                    location: -1,
                })),
            })),
            location: -1,
            ..Default::default()
        }))),
    }
}

/// The star a RETURNING item is: `Some(None)` for `*`, `Some(Some(q))` for
/// `q.*`, `None` for anything else.
fn star_of(n: &pg_query::protobuf::Node) -> Option<Option<String>> {
    let Some(N::ResTarget(rt)) = n.node.as_ref() else {
        return None;
    };
    let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) else {
        return None;
    };
    match c.fields.as_slice() {
        [s] if matches!(s.node, Some(N::AStar(_))) => Some(None),
        [q, s] if matches!(s.node, Some(N::AStar(_))) => match q.node.as_ref() {
            Some(N::String(q)) => Some(Some(q.sval.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Expand the stars in `returning`. A list whose FROM items cannot all be
/// typed here is left as it was; a `q.*` naming none of them is 42P01.
pub(crate) fn expand(
    returning: &mut Vec<pg_query::protobuf::Node>,
    relation: Option<&pg_query::protobuf::RangeVar>,
    from: &[pg_query::protobuf::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<()> {
    // A bare `*` over the target alone is already the target's columns.
    let needed = returning
        .iter()
        .any(|n| matches!(star_of(n), Some(Some(_))) || (!from.is_empty() && star_of(n).is_some()));
    if !needed {
        return Ok(());
    }
    let Some(relation) = relation else {
        return Ok(());
    };
    let Some(target) = lookup(&relation_name(relation)) else {
        return Ok(());
    };
    let target_name = relation
        .alias
        .as_ref()
        .map_or_else(|| relation.relname.clone(), |a| a.aliasname.clone());
    let mut items: Vec<(String, Vec<String>)> = vec![(
        target_name,
        target.columns.iter().map(|c| c.name.clone()).collect(),
    )];
    for item in from {
        let Some((names, def)) = correlated::from_item_def(item, lookup, params) else {
            return Ok(());
        };
        let Some(name) = names.into_iter().next() else {
            return Ok(());
        };
        items.push((name, def.columns.iter().map(|c| c.name.clone()).collect()));
    }
    let mut out = Vec::with_capacity(returning.len());
    for n in returning.drain(..) {
        match star_of(&n) {
            Some(None) => {
                for (q, cols) in &items {
                    out.extend(cols.iter().map(|c| column(q, c)));
                }
            }
            Some(Some(q)) => match items.iter().find(|(name, _)| *name == q) {
                Some((q, cols)) => out.extend(cols.iter().map(|c| column(q, c))),
                None => {
                    if let Some(at) = location(&n) {
                        set_error_location(at);
                    }
                    return Err(Error::Sqlstate(
                        "42P01",
                        format!("missing FROM-clause entry for table \"{q}\""),
                    ));
                }
            },
            None => out.push(n),
        }
    }
    *returning = out;
    Ok(())
}

fn location(n: &pg_query::protobuf::Node) -> Option<i32> {
    let Some(N::ResTarget(rt)) = n.node.as_ref() else {
        return None;
    };
    match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
        Some(N::ColumnRef(c)) if c.location >= 0 => Some(c.location),
        _ => None,
    }
}
