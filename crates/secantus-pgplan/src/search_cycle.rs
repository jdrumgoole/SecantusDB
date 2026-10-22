//! A recursive CTE's `SEARCH` and `CYCLE` clauses (RP-062), rewritten into
//! the CTE's two terms as PostgreSQL 15's `rewriteSearchAndCycle` does
//! (`rewrite/rewriteSearchCycle.c`), so the planner runs an ordinary
//! recursive CTE:
//!
//! - `SEARCH DEPTH FIRST BY c SET s`: the non-recursive term adds
//!   `ARRAY[ROW(c)]`, the recursive one `s || ARRAY[ROW(c')]`, where `c'` is
//!   that term's own expression for `c` -- the new row's value;
//! - `SEARCH BREADTH FIRST BY c SET s`: `ROW(0::int8, c)` and
//!   `ROW((s).f1 + 1, c')`;
//! - `CYCLE c SET m [TO v DEFAULT d] USING p`: `d, ARRAY[ROW(c)]` and
//!   `CASE WHEN ROW(c') = ANY (p) THEN v ELSE d END, p || ARRAY[ROW(c')]`,
//!   the recursive term reading only rows whose `m` is not `v`.
//!
//! The added columns follow the CTE's own, search column first.

use super::*;

fn string_names(list: &[pg_query::protobuf::Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// The text of an expression node, for building the rewritten terms.
fn text(n: &pg_query::protobuf::Node) -> Result<String> {
    let stmt = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(pg_query::protobuf::SelectStmt {
            target_list: vec![pg_query::protobuf::Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    val: Some(Box::new(n.clone())),
                    ..Default::default()
                }))),
            }],
            // Every enum field a real value: deparse ASSERTS on a zero one,
            // which aborts the whole server process.
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            limit_option: pg_query::protobuf::LimitOption::Default as i32,
            ..Default::default()
        }))),
    };
    let sql = stmt.deparse().map_err(|e| Error::Parse(e.to_string()))?;
    Ok(sql.strip_prefix("SELECT ").unwrap_or(&sql).to_string())
}

/// `SELECT <expr> AS <name>`'s target, parsed back into a node.
fn target(expr: &str, name: &str) -> Result<pg_query::protobuf::Node> {
    let N::SelectStmt(s) = parse_one(&format!("SELECT {expr} AS {}", quote_ident(name)))? else {
        return Err(Error::Internal("a SELECT parsed as something else".into()));
    };
    s.target_list
        .into_iter()
        .next()
        .ok_or_else(|| Error::Internal("no target".into()))
}

/// An expression, parsed.
fn expr(sql: &str) -> Result<pg_query::protobuf::Node> {
    let N::SelectStmt(s) = parse_one(&format!("SELECT {sql}"))? else {
        return Err(Error::Internal("a SELECT parsed as something else".into()));
    };
    match s.target_list.into_iter().next().and_then(|t| t.node) {
        Some(N::ResTarget(rt)) => rt
            .val
            .map(|v| *v)
            .ok_or_else(|| Error::Internal("no value".into())),
        _ => Err(Error::Internal("no target".into())),
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The name a term's target is known by (`FigureColname`, as far as a
/// recursive CTE's column needs it).
fn target_name(t: &pg_query::protobuf::Node) -> String {
    let Some(N::ResTarget(rt)) = t.node.as_ref() else {
        return "?column?".into();
    };
    if !rt.name.is_empty() {
        return rt.name.clone();
    }
    match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
        Some(N::ColumnRef(c)) => column_ref_name(c).unwrap_or_else(|| "?column?".into()),
        _ => "?column?".into(),
    }
}

/// The name the recursive term reads the CTE by: its alias, or its name.
fn self_reference(item: &pg_query::protobuf::Node, cte: &str) -> Option<String> {
    match item.node.as_ref()? {
        N::RangeVar(r) if r.relname == cte && r.schemaname.is_empty() => Some(
            r.alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone()),
        ),
        N::JoinExpr(j) => [j.larg.as_deref(), j.rarg.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|s| self_reference(s, cte)),
        _ => None,
    }
}

pub(crate) fn rewrite(cte: &mut pg_query::protobuf::CommonTableExpr) -> Result<()> {
    let search = cte.search_clause.take();
    let cycle = cte.cycle_clause.take();
    if search.is_none() && cycle.is_none() {
        return Ok(());
    }
    let refuse = || Error::Unsupported("SEARCH or CYCLE on this recursive query".into());
    if !cte.cterecursive {
        return Err(Error::Sqlstate(
            "42601",
            "WITH query is not recursive".into(),
        ));
    }
    let Some(N::SelectStmt(body)) = cte.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
    else {
        return Err(refuse());
    };
    if pg_query::protobuf::SetOperation::try_from(body.op)
        != Ok(pg_query::protobuf::SetOperation::SetopUnion)
    {
        return Err(refuse());
    }
    let (Some(larg), Some(rarg)) = (body.larg.as_deref_mut(), body.rarg.as_deref_mut()) else {
        return Err(refuse());
    };
    if !rarg.values_lists.is_empty() {
        return Err(refuse());
    }
    let mut columns = string_names(&cte.aliascolnames);
    if columns.is_empty() {
        columns = larg.target_list.iter().map(target_name).collect();
    }
    let me = rarg
        .from_clause
        .iter()
        .find_map(|f| self_reference(f, &cte.ctename))
        .ok_or_else(refuse)?;
    // A column's value in each term: the non-recursive term's own name,
    // the recursive term's expression for it.
    let recursive_expr = |name: &str| -> Result<String> {
        let i = columns.iter().position(|c| c == name).ok_or_else(refuse)?;
        let Some(N::ResTarget(rt)) = rarg.target_list.get(i).and_then(|t| t.node.as_ref()) else {
            return Err(refuse());
        };
        rt.val.as_deref().map(text).ok_or_else(refuse)?
    };
    let check_columns = |list: &[String], what: &str| -> Result<()> {
        for c in list {
            if !columns.contains(c) {
                return Err(Error::Sqlstate(
                    "42601",
                    format!("{what} column \"{c}\" not in WITH query column list"),
                ));
            }
        }
        Ok(())
    };
    // The fields of `ROW(c)` in each term.
    let fields = |cols: &[String]| {
        cols.iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let fields_rec = |cols: &[String]| -> Result<String> {
        Ok(cols
            .iter()
            .map(|c| recursive_expr(c).map(|e| format!("({e})")))
            .collect::<Result<Vec<_>>>()?
            .join(", "))
    };
    let row = |cols: &[String]| format!("ROW({})", fields(cols));
    let row_rec = |cols: &[String]| -> Result<String> { Ok(format!("ROW({})", fields_rec(cols)?)) };
    // The non-recursive term reads its own columns by name, through a
    // subquery that names them, so the new columns can use them.
    let mut left_add: Vec<(String, String)> = Vec::new();
    let mut right_add: Vec<(String, String)> = Vec::new();
    let mut right_where: Option<String> = None;
    let mut new_names: Vec<String> = Vec::new();
    if let Some(s) = &search {
        let cols = string_names(&s.search_col_list);
        check_columns(&cols, "search")?;
        let seq = &s.search_seq_column;
        if columns.contains(seq) {
            // PostgreSQL adds the column anyway; reading it is then
            // ambiguous.
            return Err(Error::Sqlstate(
                "42702",
                format!("column reference \"{seq}\" is ambiguous"),
            ));
        }
        if s.search_breadth_first {
            left_add.push((seq.clone(), format!("ROW(0::int8, {})", fields(&cols))));
            right_add.push((
                seq.clone(),
                format!("ROW(({}).f1 + 1, {})", quote_ident(seq), fields_rec(&cols)?),
            ));
        } else {
            left_add.push((seq.clone(), format!("ARRAY[{}]", row(&cols))));
            right_add.push((
                seq.clone(),
                format!(
                    "{}.{} || ARRAY[{}]",
                    quote_ident(&me),
                    quote_ident(seq),
                    row_rec(&cols)?
                ),
            ));
        }
        new_names.push(seq.clone());
    }
    if let Some(c) = &cycle {
        let cols = string_names(&c.cycle_col_list);
        check_columns(&cols, "cycle")?;
        let (mark, path) = (&c.cycle_mark_column, &c.cycle_path_column);
        for name in [mark, path] {
            if columns.contains(name) {
                return Err(Error::Sqlstate(
                    "42702",
                    format!("column reference \"{name}\" is ambiguous"),
                ));
            }
        }
        if mark == path {
            return Err(Error::Sqlstate(
                "42601",
                "cycle mark column name and cycle path column name are the same".into(),
            ));
        }
        if let Some(s) = &search {
            for (name, what) in [(mark, "mark"), (path, "path")] {
                if &s.search_seq_column == name {
                    return Err(Error::Sqlstate(
                        "42601",
                        format!(
                            "search sequence column name and cycle {what} column name are the same"
                        ),
                    ));
                }
            }
        }
        let value = c
            .cycle_mark_value
            .as_deref()
            .map(text)
            .transpose()?
            .unwrap_or_else(|| "true".into());
        let default = c
            .cycle_mark_default
            .as_deref()
            .map(text)
            .transpose()?
            .unwrap_or_else(|| "false".into());
        left_add.push((mark.clone(), default.clone()));
        left_add.push((path.clone(), format!("ARRAY[{}]", row(&cols))));
        let new_row = row_rec(&cols)?;
        right_add.push((
            mark.clone(),
            format!(
                "CASE WHEN {new_row} = ANY ({}.{}) THEN {value} ELSE {default} END",
                quote_ident(&me),
                quote_ident(path)
            ),
        ));
        right_add.push((
            path.clone(),
            format!(
                "{}.{} || ARRAY[{new_row}]",
                quote_ident(&me),
                quote_ident(path)
            ),
        ));
        right_where = Some(format!(
            "{}.{} <> {value}",
            quote_ident(&me),
            quote_ident(mark)
        ));
        new_names.push(mark.clone());
        new_names.push(path.clone());
    }
    // The non-recursive term, wrapped so its columns have the CTE's names.
    let inner = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(larg.clone()))),
    }
    .deparse()
    .map_err(|e| Error::Parse(e.to_string()))?;
    let alias_cols = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut left_sql = format!(
        "SELECT {}",
        columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (name, e) in &left_add {
        left_sql.push_str(&format!(", {e} AS {}", quote_ident(name)));
    }
    left_sql.push_str(&format!(" FROM ({inner}) AS \"*TLOCRN*\"({alias_cols})"));
    let N::SelectStmt(left) = parse_one(&left_sql)? else {
        return Err(Error::Internal("the rewritten term is not a SELECT".into()));
    };
    **body.larg.as_mut().expect("checked") = *left;
    let rarg = body.rarg.as_deref_mut().expect("checked");
    for (name, e) in &right_add {
        rarg.target_list.push(target(e, name)?);
    }
    if let Some(w) = right_where {
        let cond = expr(&w)?;
        rarg.where_clause = Some(Box::new(match rarg.where_clause.take() {
            None => cond,
            Some(old) => pg_query::protobuf::Node {
                node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                    boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                    args: vec![*old, cond],
                    ..Default::default()
                }))),
            },
        }));
    }
    if !cte.aliascolnames.is_empty() {
        for name in new_names {
            cte.aliascolnames.push(pg_query::protobuf::Node {
                node: Some(N::String(pg_query::protobuf::String { sval: name })),
            });
        }
    }
    Ok(())
}

/// Rewrite every SEARCH / CYCLE clause of this SELECT's WITH. `None` when
/// there is none.
pub(crate) fn rewrite_select(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let has = s.with_clause.as_ref().is_some_and(|w| {
        w.ctes.iter().any(|c| {
            matches!(c.node.as_ref(), Some(N::CommonTableExpr(cte))
                if cte.search_clause.is_some() || cte.cycle_clause.is_some())
        })
    });
    if !has {
        return Ok(None);
    }
    let mut out = s.clone();
    if let Some(w) = out.with_clause.as_mut() {
        for c in &mut w.ctes {
            if let Some(N::CommonTableExpr(cte)) = c.node.as_mut() {
                rewrite(cte)?;
            }
        }
    }
    Ok(Some(out))
}
