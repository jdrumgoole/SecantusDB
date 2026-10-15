//! Set-returning functions in the select list, ORDER BY, GROUP BY and
//! DISTINCT, as PostgreSQL runs them: in a ProjectSet node.
//!
//! A set-returning call changes the ROW COUNT, which no per-row expression
//! can do, so each is lifted into a LATERAL `ROWS FROM` source and the
//! expression reads its column (`select unnest(ia) from t` is `from t,
//! lateral unnest(ia)`). Measured against PostgreSQL 15.19:
//!
//! - Several calls at one nesting level run in LOCKSTEP: the longest decides
//!   the row count and the others pad with NULL -- one `ROWS FROM` of them
//!   all. A call whose arguments hold another call runs one level above it
//!   (`generate_series(1, generate_series(1, 2))` is 1, 1, 2), so each level
//!   is its own `ROWS FROM`, lateral to the one below
//!   (`split_pathtarget_at_srfs`).
//! - An ORDER BY or DISTINCT call is a resjunk target of the same ProjectSet,
//!   and the same call text is the same target (`findTargetlistEntrySQL99`).
//!   A GROUP BY call is evaluated below the grouping.
//! - The ProjectSet sits ABOVE aggregation and windows: `select count(*),
//!   generate_series(1, 3) from t` is the count three times, and `count(*)
//!   over ()` counts the input rows, not the generated ones. Such a query is
//!   split into the grouped / windowed query as a subquery and the
//!   set-returning targets over it.
//! - A record-returning call (`json_each`, a function `RETURNS SETOF` a
//!   composite or with OUT parameters) is a row of its fields: `(f()).key`
//!   reads one, `(f()).*` expands to all, and `f()` alone is the record.

use super::*;
use pg_query::protobuf::{Node, SelectStmt};

mod split;

/// What one set-returning call yields per row.
#[derive(Clone)]
enum Kind {
    Scalar,
    /// A row: its fields' names, and the composite type it is of (`None`: an
    /// anonymous `record`).
    Record {
        fields: Vec<String>,
        rowtype: Option<String>,
    },
}

/// The JSON functions returning `setof record` with OUT `key` and `value`.
const JSON_EACH: &[&str] = &[
    "json_each",
    "jsonb_each",
    "json_each_text",
    "jsonb_each_text",
];

fn srf_kind(n: &Node) -> Option<Kind> {
    let Some(N::FuncCall(f)) = n.node.as_ref() else {
        return None;
    };
    if f.over.is_some() {
        return None;
    }
    let name = func_name(f)?;
    if JSON_EACH.contains(&name.as_str()) {
        return Some(Kind::Record {
            fields: vec!["key".into(), "value".into()],
            rowtype: None,
        });
    }
    // A populate function's rows are of its base argument's composite type.
    if matches!(
        name.as_str(),
        "json_populate_recordset" | "jsonb_populate_recordset"
    ) {
        let ty = static_type(f.args.first()?, &Bson::Null);
        let (_, fields) = user_composite(&ty)?;
        return Some(Kind::Record {
            fields: fields.into_iter().map(|(n, _)| n).collect(),
            rowtype: Some(ty),
        });
    }
    if joins::SELECT_LIST_SRFS.contains(&name.as_str()) {
        return Some(Kind::Scalar);
    }
    let u = correlated::user_function_for(&name, &f.args).filter(|u| u.returns_set)?;
    if !u.columns.is_empty() {
        return Some(Kind::Record {
            fields: u.columns.iter().map(|(n, _)| n.clone()).collect(),
            rowtype: None,
        });
    }
    Some(match user_composite(&u.return_type) {
        Some((_, fields)) => Kind::Record {
            fields: fields.into_iter().map(|(n, _)| n).collect(),
            rowtype: Some(u.return_type),
        },
        None => Kind::Scalar,
    })
}

/// Whether the expression holds a set-returning call (not counting one
/// inside a subquery, which belongs to the subquery).
fn contains_srf(n: &Node) -> bool {
    let mut probe = n.clone();
    let mut hit = false;
    let _ = walk_expr(&mut probe, &mut |m| {
        hit |= srf_kind(m).is_some();
        Ok(())
    });
    hit
}

/// A call whose arguments hold no other set-returning call: the next level.
fn is_leaf(n: &Node) -> bool {
    match n.node.as_ref() {
        Some(N::FuncCall(f)) => srf_kind(n).is_some() && !f.args.iter().any(contains_srf),
        _ => false,
    }
}

/// Every expression a ProjectSet (or the grouping below it) evaluates.
fn for_each_site(s: &mut SelectStmt, f: &mut dyn FnMut(&mut Node) -> Result<()>) -> Result<()> {
    for t in &mut s.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                f(v)?;
            }
        }
    }
    for item in &mut s.sort_clause {
        if let Some(N::SortBy(sb)) = item.node.as_mut() {
            if let Some(v) = sb.node.as_deref_mut() {
                f(v)?;
            }
        }
    }
    for item in s
        .group_clause
        .iter_mut()
        .chain(s.distinct_clause.iter_mut())
    {
        if item.node.is_some() {
            f(item)?;
        }
    }
    Ok(())
}

fn has_srf_site(s: &SelectStmt) -> bool {
    let mut probe = s.clone();
    let mut hit = false;
    let _ = for_each_site(&mut probe, &mut |n| {
        hit |= contains_srf(n);
        Ok(())
    });
    hit
}

fn column(rel: &str, col: &str) -> Node {
    Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node(rel), string_node(col)],
            location: -1,
        })),
    }
}

/// One SQL expression as its parse tree.
fn parse_expression(sql: &str) -> Result<Node> {
    match parse_one(&format!("SELECT {sql}"))? {
        N::SelectStmt(s) => match s.target_list.first().and_then(|t| t.node.as_ref()) {
            Some(N::ResTarget(rt)) => rt
                .val
                .as_deref()
                .cloned()
                .ok_or_else(|| Error::Internal("set-returning rewrite".into())),
            _ => Err(Error::Internal("set-returning rewrite".into())),
        },
        _ => Err(Error::Internal("set-returning rewrite".into())),
    }
}

/// `select f(...)` with no FROM and nothing else is its own source, planned
/// by `plan_select_srf` / `plan_select_unnest` -- except a function of a
/// composite row type, which those do not type.
fn bare_single(s: &SelectStmt) -> bool {
    let [t] = s.target_list.as_slice() else {
        return false;
    };
    let Some(N::ResTarget(rt)) = t.node.as_ref() else {
        return false;
    };
    let Some(v) = rt.val.as_deref() else {
        return false;
    };
    let mut rest = s.clone();
    rest.target_list.clear();
    s.from_clause.is_empty()
        && is_leaf(v)
        && (!matches!(
            srf_kind(v),
            Some(Kind::Record {
                rowtype: Some(_),
                ..
            })
        ) || matches!(v.node.as_ref(), Some(N::FuncCall(f))
                if func_name(f).is_some_and(|n| n.ends_with("populate_recordset"))))
        && !has_srf_site(&rest)
}

/// The select with its set-returning calls lifted into LATERAL sources, or
/// `None` when it has none to lift.
pub(crate) fn rewrite(s: &SelectStmt) -> Result<Option<SelectStmt>> {
    if !has_srf_site(s) || bare_single(s) {
        return Ok(None);
    }
    let mut out = s.clone();
    expand_record_stars(&mut out);
    // The output keeps the name PostgreSQL gives the ORIGINAL expression
    // (`jsonb_path_query` for its `::text` too, `key` for `(json_each(j)).key`).
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if rt.name.is_empty() {
                if let Some(v) = rt.val.as_deref().filter(|v| contains_srf(v)) {
                    rt.name = expression_column_name(v);
                }
            }
        }
    }
    let aggregated = has_aggregate(&out)
        || !out.group_clause.is_empty()
        || out.having_clause.is_some()
        || has_window(&out)
        || out.sort_clause.iter().any(|item| match item.node.as_ref() {
            Some(N::SortBy(sb)) => node_has_window(sb.node.as_deref()),
            _ => false,
        });
    if aggregated {
        if let Some(split) = split::split_above_grouping(&out)? {
            return Ok(Some(split));
        }
    }
    if out.from_clause.is_empty() {
        // One row for the calls to join to.
        let N::SelectStmt(one) = parse_one("SELECT FROM (SELECT) __one")? else {
            return Err(Error::Internal("set-returning rewrite".into()));
        };
        out.from_clause = one.from_clause;
    }
    let mut level = 0;
    loop {
        // This level's calls, each distinct call text once, in order.
        let mut calls: Vec<(String, Node, Kind)> = Vec::new();
        let mut probe = out.clone();
        for_each_site(&mut probe, &mut |site| {
            walk_expr(site, &mut |m| {
                if is_leaf(m) {
                    let key = deparse_expr(m)?;
                    if !calls.iter().any(|(k, _, _)| *k == key) {
                        let kind = srf_kind(m).expect("a leaf");
                        calls.push((key, m.clone(), kind));
                    }
                }
                Ok(())
            })
        })?;
        if calls.is_empty() {
            break;
        }
        let alias = format!("__srf{level}");
        let name = |k: usize, field: Option<usize>| match field {
            Some(i) => format!("c{k}_{i}"),
            None => format!("c{k}"),
        };
        let find = |m: &Node| -> Result<Option<usize>> {
            if !is_leaf(m) {
                return Ok(None);
            }
            let key = deparse_expr(m)?;
            Ok(calls.iter().position(|(k, _, _)| *k == key))
        };
        for_each_site(&mut out, &mut |site| {
            walk_expr(site, &mut |m| {
                // `(f()).field` reads that field's column.
                if let Some(N::AIndirection(ind)) = m.node.as_ref() {
                    let field = match ind.indirection.first().and_then(|n| n.node.as_ref()) {
                        Some(N::String(sv)) => Some(sv.sval.clone()),
                        _ => None,
                    };
                    if let (Some(arg), Some(field)) = (ind.arg.as_deref(), field) {
                        if let Some(k) = find(arg)? {
                            if let Kind::Record { fields, rowtype } = &calls[k].2 {
                                let i = fields.iter().position(|f| *f == field).ok_or_else(|| {
                                    Error::UndefinedField(match rowtype {
                                        Some(t) => {
                                            format!("column \"{field}\" not found in data type {t}")
                                        }
                                        None => format!(
                                            "could not identify column \"{field}\" in record data type"
                                        ),
                                    })
                                })?;
                                let col = column(&alias, &name(k, Some(i)));
                                *m = if ind.indirection.len() > 1 {
                                    Node {
                                        node: Some(N::AIndirection(Box::new(
                                            pg_query::protobuf::AIndirection {
                                                arg: Some(Box::new(col)),
                                                indirection: ind.indirection[1..].to_vec(),
                                            },
                                        ))),
                                    }
                                } else {
                                    col
                                };
                                return Ok(());
                            }
                        }
                    }
                }
                let Some(k) = find(m)? else {
                    return Ok(());
                };
                *m = match &calls[k].2 {
                    Kind::Scalar => column(&alias, &name(k, None)),
                    // The call alone is its whole row.
                    Kind::Record { fields, rowtype } => {
                        let cols: Vec<String> = (0..fields.len())
                            .map(|i| format!("{alias}.{}", name(k, Some(i))))
                            .collect();
                        let cast = rowtype.as_ref().map_or(String::new(), |t| format!("::{t}"));
                        let mut row = parse_expression(&format!("ROW({}){cast}", cols.join(", ")))?;
                        // An anonymous record still knows its fields' names
                        // (`row_to_json(json_each(j))` is `{"key":..,"value":..}`).
                        if let Some(N::RowExpr(r)) = row.node.as_mut() {
                            r.colnames = fields.iter().map(|f| string_node(f)).collect();
                        }
                        row
                    }
                };
                Ok(())
            })
        })?;
        let mut colnames = Vec::new();
        for (k, (_, _, kind)) in calls.iter().enumerate() {
            match kind {
                Kind::Scalar => colnames.push(string_node(&name(k, None))),
                Kind::Record { fields, .. } => {
                    colnames.extend((0..fields.len()).map(|i| string_node(&name(k, Some(i)))));
                }
            }
        }
        let rf = pg_query::protobuf::RangeFunction {
            lateral: true,
            is_rowsfrom: calls.len() > 1,
            functions: calls
                .into_iter()
                .map(|(_, call, _)| Node {
                    node: Some(N::List(pg_query::protobuf::List {
                        items: vec![call, Node { node: None }],
                    })),
                })
                .collect(),
            alias: Some(pg_query::protobuf::Alias {
                aliasname: alias,
                colnames,
            }),
            ..Default::default()
        };
        out.from_clause.push(Node {
            node: Some(N::RangeFunction(rf)),
        });
        level += 1;
    }
    Ok(Some(out))
}

/// `(f()).*` over a record-returning call: one target per field, named for
/// it, each reading the one call.
fn expand_record_stars(s: &mut SelectStmt) {
    let mut targets = Vec::with_capacity(s.target_list.len());
    for t in std::mem::take(&mut s.target_list) {
        let expanded = match t.node.as_ref() {
            Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                Some(N::AIndirection(ind))
                    if matches!(
                        ind.indirection.as_slice(),
                        [Node {
                            node: Some(N::AStar(_))
                        }]
                    ) =>
                {
                    match ind.arg.as_deref().map(|a| (a, srf_kind(a))) {
                        Some((arg, Some(Kind::Record { fields, .. }))) => Some(
                            fields
                                .iter()
                                .map(|f| Node {
                                    node: Some(N::ResTarget(Box::new(
                                        pg_query::protobuf::ResTarget {
                                            name: f.clone(),
                                            val: Some(Box::new(Node {
                                                node: Some(N::AIndirection(Box::new(
                                                    pg_query::protobuf::AIndirection {
                                                        arg: Some(Box::new(arg.clone())),
                                                        indirection: vec![string_node(f)],
                                                    },
                                                ))),
                                            })),
                                            location: rt.location,
                                            ..Default::default()
                                        },
                                    ))),
                                })
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        };
        match expanded {
            Some(e) => targets.extend(e),
            None => targets.push(t),
        }
    }
    s.target_list = targets;
}
