//! `UPDATE ... SET (a, b) = (SELECT x, y ...)`: several columns assigned from
//! one row of a subquery.
//!
//! PostgreSQL evaluates the subquery ONCE per updated row (once per
//! statement when it is uncorrelated, as an initplan) and assigns its columns
//! in order: no row sets every listed column NULL, two rows are 21000, and a
//! column count that differs from the list is 42601 at the subquery's `(`
//! (measured on 15.19). Here the subquery is turned into one that yields a
//! single RECORD, `(SELECT ROW(x, y) ...)`, and each listed column reads one
//! field of it. `rewrite` builds that subquery before the statement's
//! subqueries are resolved, so it is run once (uncorrelated) or becomes a
//! per-row call (correlated) like any other; it stands as the source of the
//! list's FIRST column, and `plan_update` carries it to the rest.
//! `update_row_sets` evaluates a per-row record once for the whole list
//! (`RecordMemo`), so a volatile subquery runs once, as there.

use super::*;

/// The alias of the wrapping FROM item when the subquery's own targets cannot
/// be rewritten. A unit separator keeps it out of any name a user can write.
const WRAP_ALIAS: &str = "\u{1f}multiassign";

/// One `SET (...) = (SELECT ...)` list as `plan_update` sees it: the
/// record-valued source of its first column, resolved.
pub(crate) struct Group {
    pub record: pg_query::protobuf::Node,
    pub ncolumns: usize,
    /// The subquery's `(`, where a width that differs is reported.
    pub location: i32,
    /// The record, evaluated once, for an uncorrelated subquery.
    pub value: Option<Bson>,
}

/// The record subquery and its own targets (when known before it runs).
struct Built {
    record: pg_query::protobuf::Node,
    targets: Option<Vec<pg_query::protobuf::Node>>,
}

/// Turn each `SET (...) = (SELECT ...)` list of `u` into one record
/// subquery standing as its first column's source (the others' sources are
/// cleared), checking the column count and each target's type first.
pub(crate) fn rewrite(
    u: &mut pg_query::protobuf::UpdateStmt,
    def: Option<&TableDef>,
) -> Result<()> {
    let mut targets: Option<Vec<pg_query::protobuf::Node>> = None;
    let mut in_list = false;
    for t in &mut u.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_mut() else {
            continue;
        };
        let name = rt.name.clone();
        let Some(N::MultiAssignRef(m)) = rt.val.as_deref_mut().and_then(|v| v.node.as_mut()) else {
            in_list = false;
            continue;
        };
        if m.colno == 1 {
            in_list = matches!(
                m.source.as_deref().and_then(|s| s.node.as_ref()),
                Some(N::SubLink(_))
            );
            if !in_list {
                continue;
            }
            let built = build(m)?;
            m.source = Some(Box::new(built.record));
            targets = built.targets;
        } else if in_list {
            m.source = None;
        } else {
            continue;
        }
        let i = usize::try_from(m.colno - 1).unwrap_or(0);
        let (Some(column), Some(target)) = (
            def.and_then(|d| d.column(&name)),
            targets.as_ref().and_then(|ts| ts.get(i)),
        ) else {
            continue;
        };
        // An untyped literal in the subquery's list is `text` there.
        let untyped = matches!(
            target.node.as_ref(),
            Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
        );
        if untyped {
            assignable(column, "text", "expression").inspect_err(|_| {
                if let Some(loc) = expr_location(target) {
                    set_error_location(loc);
                }
            })?;
        } else {
            check_assignment_type(column, target)?;
        }
    }
    Ok(())
}

fn node(n: N) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node { node: Some(n) }
}

fn mismatch(location: i32) -> Error {
    if location >= 0 {
        set_error_location(location);
    }
    Error::Sqlstate(
        "42601",
        "number of columns does not match number of values".into(),
    )
}

/// The record subquery for the list whose first column `m` is.
fn build(m: &pg_query::protobuf::MultiAssignRef) -> Result<Built> {
    let Some(N::SubLink(sub)) = m.source.as_deref().and_then(|s| s.node.as_ref()) else {
        return Err(Error::Unsupported(
            "this multiple-column UPDATE source".into(),
        ));
    };
    let ncolumns = usize::try_from(m.ncolumns).unwrap_or(0);
    let Some(N::SelectStmt(sel)) = sub.subselect.as_deref().and_then(|s| s.node.as_ref()) else {
        return Err(Error::Unsupported(
            "this multiple-column UPDATE source".into(),
        ));
    };
    let star = sel.target_list.iter().any(|t| match t.node.as_ref() {
        Some(N::ResTarget(rt)) => matches!(
            rt.val.as_deref().and_then(|v| v.node.as_ref()),
            Some(N::ColumnRef(c)) if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_))))
        ),
        _ => false,
    });
    // A plain SELECT has its targets rewritten in place, so a correlated
    // reference stays where it was. Anything an output column can be named
    // by (ORDER BY 1, GROUP BY 1, DISTINCT, a set operation, `*`, VALUES)
    // keeps its targets and is read whole through a FROM item instead.
    let plain = sel.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && sel.values_lists.is_empty()
        && sel.sort_clause.is_empty()
        && sel.group_clause.is_empty()
        && sel.distinct_clause.is_empty()
        && !star;
    let (select, targets) = if plain {
        let vals: Vec<pg_query::protobuf::Node> = sel
            .target_list
            .iter()
            .filter_map(|t| match t.node.as_ref() {
                Some(N::ResTarget(rt)) => rt.val.as_deref().cloned(),
                _ => None,
            })
            .collect();
        if vals.len() != ncolumns {
            return Err(mismatch(sub.location));
        }
        let mut s = (**sel).clone();
        s.target_list = vec![res_target(row(vals.clone()))];
        (s, Some(vals))
    } else {
        let whole = node(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![node(N::String(pg_query::protobuf::String {
                sval: WRAP_ALIAS.into(),
            }))],
            location: -1,
        }));
        let from = node(N::RangeSubselect(Box::new(
            pg_query::protobuf::RangeSubselect {
                subquery: sub.subselect.clone(),
                // Named, so the whole-row value is read by position: two
                // unnamed columns (`select 1, 'a' union ...`) share a name.
                alias: Some(pg_query::protobuf::Alias {
                    aliasname: WRAP_ALIAS.into(),
                    colnames: (1..=ncolumns)
                        .map(|i| {
                            node(N::String(pg_query::protobuf::String {
                                sval: format!("f{i}"),
                            }))
                        })
                        .collect(),
                }),
                ..Default::default()
            },
        )));
        let s = pg_query::protobuf::SelectStmt {
            target_list: vec![res_target(whole)],
            from_clause: vec![from],
            limit_option: pg_query::protobuf::LimitOption::Default as i32,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        };
        (s, None)
    };
    let record = node(N::SubLink(Box::new(pg_query::protobuf::SubLink {
        sub_link_type: pg_query::protobuf::SubLinkType::ExprSublink as i32,
        subselect: Some(Box::new(node(N::SelectStmt(Box::new(select))))),
        location: sub.location,
        ..(**sub).clone()
    })));
    Ok(Built { record, targets })
}

fn res_target(val: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    node(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
        val: Some(Box::new(val)),
        location: -1,
        ..Default::default()
    })))
}

fn row(args: Vec<pg_query::protobuf::Node>) -> pg_query::protobuf::Node {
    node(N::RowExpr(Box::new(pg_query::protobuf::RowExpr {
        args,
        row_format: pg_query::protobuf::CoercionForm::CoerceImplicitCast as i32,
        location: -1,
        ..Default::default()
    })))
}

/// The value assigned to the list's column `i` (0-based): field `i` of the
/// record, NULL when the subquery returned no row. A record of another width
/// comes from a subquery whose columns were not known before it ran (`*`, a
/// set operation).
pub(crate) fn field(record: &Bson, i: usize, ncolumns: usize, location: i32) -> Result<Bson> {
    if *record == Bson::Null {
        return Ok(Bson::Null);
    }
    let values = record_values(record).ok_or_else(|| mismatch(location))?;
    if values.len() != ncolumns {
        return Err(mismatch(location));
    }
    Ok(values.get(i).cloned().unwrap_or(Bson::Null))
}

/// The type of field `i`, as the record recorded it or as its value says.
pub(crate) fn field_type(record: &Bson, i: usize, value: &Bson) -> String {
    match record_field_types(record).and_then(|t| t.get(i).cloned()) {
        Some(t) if !t.is_empty() => t,
        _ => static_type(&pg_query::protobuf::Node::default(), value),
    }
}

/// The `result_type` of a set expression that is field `i` of a per-row
/// record (the expression itself is the record's). It is never a type a
/// value can have, and `update_row_sets` reads the field from it.
const FIELD_MARKER: &str = "\u{1f}field:";

pub(crate) fn field_marker(i: usize, ncolumns: usize, location: i32) -> String {
    format!("{FIELD_MARKER}{i}/{ncolumns}/{location}")
}

/// `(i, ncolumns, location)` from a `field_marker`.
pub(crate) fn parse_marker(result_type: &str) -> Option<(usize, usize, i32)> {
    let mut parts = result_type.strip_prefix(FIELD_MARKER)?.split('/');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

/// Per-row evaluation of the lists' records: the last record expression and
/// its value, reused while the following entries name the same one.
#[derive(Default)]
pub(crate) struct RecordMemo {
    last: Option<(ColumnExpr, Bson)>,
}

impl RecordMemo {
    pub(crate) fn value(&mut self, expr: &ColumnExpr, row: &Document) -> Result<Bson> {
        if let Some((e, v)) = &self.last {
            if e == expr {
                return Ok(v.clone());
            }
        }
        let v = apply_row_expr(expr, row)?;
        self.last = Some((expr.clone(), v.clone()));
        Ok(v)
    }
}
