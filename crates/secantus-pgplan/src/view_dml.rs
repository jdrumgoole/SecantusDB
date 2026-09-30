//! Writing THROUGH a view: PostgreSQL's automatically updatable views.
//!
//! A view over ONE relation with no DISTINCT, GROUP BY, HAVING, LIMIT /
//! OFFSET, window, set operation, WITH, or aggregate / window /
//! set-returning function in its select list takes `INSERT`, `UPDATE` and
//! `DELETE`. The statement is rewritten onto the base relation -- which is
//! what PostgreSQL's rewriter does too:
//!
//! * a view column that is a bare column reference writes that column; any
//!   other view column is read-only (`0A000 cannot insert into column "x"
//!   of view "v"`), though it may still be READ in a WHERE or RETURNING;
//! * `UPDATE` / `DELETE` see only the rows the view shows, so the view's
//!   WHERE is ANDed onto the statement's;
//! * `INSERT` ignores the view's WHERE (only `WITH CHECK OPTION` applies
//!   it, and that is refused here rather than half-enforced);
//! * a view over a view rewrites again, until the target is a table.
//!
//! Every reference to a view column is replaced by the expression behind it,
//! qualified by the base relation, so a substituted reference is never
//! substituted twice.

use super::*;

thread_local! {
    /// The views created `WITH [LOCAL | CASCADED] CHECK OPTION`, and which.
    static CHECKED_VIEWS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The CHECK OPTION conditions the statement being planned must hold its
    /// new rows to: `(view, condition SQL over the base table)`. Filled by
    /// `rewrite`, drained into the plan by `take_view_checks`.
    static PENDING: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Per view, its `(column, default SQL)` pairs.
type ViewDefaults = Vec<(String, Vec<(String, String)>)>;

thread_local! {
    /// `ALTER VIEW ... ALTER COLUMN c SET DEFAULT`: per view, `(column,
    /// default SQL)`, which an INSERT through the view gives a column it
    /// omits (or writes as DEFAULT).
    static VIEW_DEFAULTS: std::cell::RefCell<ViewDefaults> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the views' column defaults for the statements that follow.
pub fn set_view_defaults(defaults: Vec<(String, Vec<(String, String)>)>) {
    VIEW_DEFAULTS.with(|d| *d.borrow_mut() = defaults);
}

fn view_defaults(view: &str) -> Vec<(String, String)> {
    VIEW_DEFAULTS.with(|d| {
        d.borrow()
            .iter()
            .find(|(v, _)| v == view)
            .map(|(_, cols)| cols.clone())
            .unwrap_or_default()
    })
}

/// Give an INSERT through `view` the view's column defaults: a column the
/// INSERT omits gets its default expression (per VALUES row, or as an extra
/// select-list entry), and an explicit DEFAULT in its slot becomes it.
fn apply_view_defaults(i: &mut pg_query::protobuf::InsertStmt, view: &str) -> Result<()> {
    let defaults = view_defaults(view);
    if defaults.is_empty() {
        return Ok(());
    }
    let parse = |sql: &str| domains::parse_default_sql(sql);
    let named: Vec<String> = i
        .cols
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
            _ => None,
        })
        .collect();
    let Some(N::SelectStmt(sel)) = i.select_stmt.as_deref_mut().and_then(|s| s.node.as_mut())
    else {
        return Ok(());
    };
    // An explicit DEFAULT in a VALUES row, at a column with a view default.
    for row in &mut sel.values_lists {
        if let Some(N::List(l)) = row.node.as_mut() {
            for (slot, item) in l.items.iter_mut().enumerate() {
                if !matches!(item.node, Some(N::SetToDefault(_))) {
                    continue;
                }
                if let Some((_, sql)) = named
                    .get(slot)
                    .and_then(|n| defaults.iter().find(|(c, _)| c == n))
                {
                    *item = parse(sql)?;
                }
            }
        }
    }
    let star = sel.target_list.iter().any(|t| {
        matches!(t.node.as_ref(), Some(N::ResTarget(rt))
            if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()),
                Some(N::ColumnRef(c)) if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_))))))
    });
    for (column, sql) in &defaults {
        if named.contains(column) {
            continue;
        }
        if !sel.values_lists.is_empty() {
            for row in &mut sel.values_lists {
                if let Some(N::List(l)) = row.node.as_mut() {
                    l.items.push(parse(sql)?);
                }
            }
        } else if !star && !sel.target_list.is_empty() {
            sel.target_list.push(pg_query::protobuf::Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    val: Some(Box::new(parse(sql)?)),
                    location: -1,
                    ..Default::default()
                }))),
            });
        } else {
            continue;
        }
        i.cols.push(pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                name: column.clone(),
                location: -1,
                ..Default::default()
            }))),
        });
    }
    Ok(())
}

/// Install the views that carry a CHECK OPTION: `(name, LOCAL | CASCADED)`.
pub fn set_checked_views(views: Vec<(String, String)>) {
    CHECKED_VIEWS.with(|v| *v.borrow_mut() = views);
}

/// Add a condition new rows must hold (row-level security's `WITH CHECK`).
pub(crate) fn add_check(name: String, condition: String) {
    PENDING.with(|p| p.borrow_mut().push((name, condition)));
}

/// The CHECK OPTION conditions the last rewrite produced.
pub(crate) fn take_view_checks() -> Vec<(String, String)> {
    PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

fn check_kind(view: &str) -> Option<String> {
    CHECKED_VIEWS.with(|v| {
        v.borrow()
            .iter()
            .find(|(n, _)| n == view)
            .map(|(_, k)| k.clone())
    })
}

/// An expression's SQL, with every column reference reduced to its bare
/// name -- the form a CHECK constraint's expression is stored and planned in.
fn condition_sql(mut expr: pg_query::protobuf::Node) -> Result<String> {
    walk_expr(&mut expr, &mut |n| {
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            if let Some(parts) = ref_parts(c) {
                let col = parts.last().cloned().unwrap_or_default();
                *n = column_ref(&[col.as_str()]);
            }
        }
        Ok(())
    })?;
    let select = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(pg_query::protobuf::SelectStmt {
            target_list: vec![pg_query::protobuf::Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    val: Some(Box::new(expr)),
                    location: -1,
                    ..Default::default()
                }))),
            }],
            limit_option: pg_query::protobuf::LimitOption::Default as i32,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        }))),
    };
    let sql = select.deparse().map_err(|e| Error::Parse(e.to_string()))?;
    Ok(sql.strip_prefix("SELECT ").unwrap_or(&sql).to_string())
}

struct ViewColumn {
    name: String,
    expr: pg_query::protobuf::Node,
    /// The base column this view column IS, when it is a bare reference.
    base: Option<String>,
}

fn column_ref(parts: &[&str]) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: parts.iter().map(|p| string_node(p)).collect(),
            location: -1,
        })),
    }
}

fn ref_parts(c: &pg_query::protobuf::ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A view created with a column list (`CREATE VIEW v (a, b) AS ...`) is
/// stored as `SELECT * FROM (<body>) AS v(a, b)`; that is the body with its
/// output columns renamed, which is what updatability is judged on.
fn unwrap_column_list(body: pg_query::protobuf::SelectStmt) -> pg_query::protobuf::SelectStmt {
    let star_only = matches!(body.target_list.as_slice(), [t] if matches!(
        t.node.as_ref(),
        Some(N::ResTarget(rt)) if matches!(
            rt.val.as_deref().and_then(|v| v.node.as_ref()),
            Some(N::ColumnRef(c)) if matches!(c.fields.as_slice(), [f] if matches!(f.node.as_ref(), Some(N::AStar(_))))
        )
    ));
    let plain = body.where_clause.is_none()
        && body.group_clause.is_empty()
        && body.distinct_clause.is_empty()
        && body.limit_count.is_none()
        && body.op == pg_query::protobuf::SetOperation::SetopNone as i32;
    let [item] = body.from_clause.as_slice() else {
        return body;
    };
    let Some(N::RangeSubselect(rs)) = item.node.as_ref() else {
        return body;
    };
    let Some(alias) = rs.alias.as_ref().filter(|a| !a.colnames.is_empty()) else {
        return body;
    };
    let Some(N::SelectStmt(inner)) = rs.subquery.as_deref().and_then(|q| q.node.as_ref()) else {
        return body;
    };
    if !star_only || !plain {
        return body;
    }
    let mut inner = (**inner).clone();
    for (t, name) in inner.target_list.iter_mut().zip(&alias.colnames) {
        if let (Some(N::ResTarget(rt)), Some(N::String(s))) = (t.node.as_mut(), name.node.as_ref())
        {
            rt.name = s.sval.clone();
        }
    }
    inner
}

/// Whether a view with this definition is automatically updatable
/// (`information_schema.views.is_updatable`).
pub fn is_updatable(definition: &str) -> bool {
    let Ok(N::SelectStmt(body)) = parse_one(definition) else {
        return false;
    };
    not_updatable(&unwrap_column_list(*body)).is_none()
}

/// Why a view is not automatically updatable, or `None` when it is.
fn not_updatable(body: &pg_query::protobuf::SelectStmt) -> Option<&'static str> {
    if body.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
        return Some("set operations");
    }
    if !body.values_lists.is_empty() {
        return Some("VALUES");
    }
    if !body.distinct_clause.is_empty() {
        return Some("DISTINCT");
    }
    if !body.group_clause.is_empty() || has_aggregate(body) {
        return Some("GROUP BY or aggregates");
    }
    if body.having_clause.is_some() {
        return Some("HAVING");
    }
    if !body.window_clause.is_empty() || has_window(body) {
        return Some("window functions");
    }
    if body.limit_count.is_some() || body.limit_offset.is_some() {
        return Some("LIMIT or OFFSET");
    }
    if body.with_clause.is_some() {
        return Some("WITH");
    }
    match body.from_clause.as_slice() {
        [item] if matches!(item.node.as_ref(), Some(N::RangeVar(_))) => None,
        _ => Some("more than one relation, or no relation"),
    }
}

/// The view's columns, in order, from its definition over `base`.
fn view_columns(
    body: &pg_query::protobuf::SelectStmt,
    base: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Vec<ViewColumn>> {
    let mut out = Vec::new();
    for t in &body.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            continue;
        };
        let Some(val) = rt.val.as_deref() else {
            continue;
        };
        if let Some(N::ColumnRef(c)) = val.node.as_ref() {
            if matches!(
                c.fields.last().and_then(|f| f.node.as_ref()),
                Some(N::AStar(_))
            ) {
                let def = lookup(base).ok_or_else(|| Error::UndefinedTable(base.to_string()))?;
                for col in &def.columns {
                    out.push(ViewColumn {
                        name: col.name.clone(),
                        expr: column_ref(&[col.name.as_str()]),
                        base: Some(col.name.clone()),
                    });
                }
                continue;
            }
            if let Some(parts) = ref_parts(c) {
                let col = parts.last().cloned().unwrap_or_default();
                out.push(ViewColumn {
                    name: if rt.name.is_empty() {
                        col.clone()
                    } else {
                        rt.name.clone()
                    },
                    expr: column_ref(&[col.as_str()]),
                    base: Some(col),
                });
                continue;
            }
        }
        out.push(ViewColumn {
            name: if rt.name.is_empty() {
                expression_column_name(val)
            } else {
                rt.name.clone()
            },
            expr: val.clone(),
            base: None,
        });
    }
    Ok(out)
}

/// Qualify every column reference in `expr` by `qualifier` (the base
/// relation's name or alias), dropping whatever qualifier it had.
fn qualify(
    mut expr: pg_query::protobuf::Node,
    qualifier: &str,
) -> Result<pg_query::protobuf::Node> {
    walk_expr(&mut expr, &mut |n| {
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            if let Some(parts) = ref_parts(c) {
                let col = parts.last().cloned().unwrap_or_default();
                *n = column_ref(&[qualifier, col.as_str()]);
            }
        }
        Ok(())
    })?;
    Ok(expr)
}

/// Replace each reference to a view column -- bare, or qualified by the
/// view's name or the statement's alias for it -- with the column's
/// expression over the base relation.
fn substitute(
    node: &mut pg_query::protobuf::Node,
    columns: &[ViewColumn],
    view_names: &[String],
    qualifier: &str,
) -> Result<()> {
    walk_expr(node, &mut |n| {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        let Some(parts) = ref_parts(c) else {
            return Ok(());
        };
        let name = match parts.as_slice() {
            [col] => col,
            [q, col] if view_names.contains(q) => col,
            _ => return Ok(()),
        };
        if let Some(vc) = columns.iter().find(|vc| &vc.name == name) {
            *n = qualify(vc.expr.clone(), qualifier)?;
        }
        Ok(())
    })
}

fn and(
    a: Option<pg_query::protobuf::Node>,
    b: Option<pg_query::protobuf::Node>,
) -> Option<Box<pg_query::protobuf::Node>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Box::new(pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: BoolExprType::AndExpr as i32,
                args: vec![a, b],
                location: -1,
                ..Default::default()
            }))),
        })),
        (Some(x), None) | (None, Some(x)) => Some(Box::new(x)),
        (None, None) => None,
    }
}

/// Rewrite an `INSERT` / `UPDATE` / `DELETE` whose target is a view onto
/// the view's base relation; a no-op for any other statement.
pub(crate) fn rewrite(
    node: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    PENDING.with(|p| p.borrow_mut().clear());
    // Conditions gathered so far, each over the CURRENT level's relation.
    let mut checks: Vec<(String, pg_query::protobuf::Node)> = Vec::new();
    let mut cascade = false;
    let finish = |checks: Vec<(String, pg_query::protobuf::Node)>| -> Result<()> {
        let out = checks
            .into_iter()
            .map(|(v, c)| Ok((v, condition_sql(c)?)))
            .collect::<Result<Vec<_>>>()?;
        PENDING.with(|p| *p.borrow_mut() = out);
        Ok(())
    };
    for _ in 0..32 {
        let relation = match node.node.as_ref() {
            Some(N::InsertStmt(i)) => i.relation.clone(),
            Some(N::UpdateStmt(u)) => u.relation.clone(),
            Some(N::DeleteStmt(d)) => d.relation.clone(),
            _ => return Ok(()),
        };
        let Some(relation) = relation else {
            return finish(checks);
        };
        if !(relation.schemaname.is_empty() || relation.schemaname == "public") {
            return finish(checks);
        }
        let view = relation.relname.clone();
        let Some(definition) = view_definition(&view) else {
            return finish(checks);
        };
        let (verb, what) = match node.node.as_ref() {
            Some(N::InsertStmt(_)) => ("insert into", "insert into"),
            Some(N::UpdateStmt(_)) => ("update", "update"),
            _ => ("delete from", "delete from"),
        };
        let N::SelectStmt(body) = parse_one(&definition)? else {
            return Err(Error::Internal(format!(
                "the stored definition of view \"{view}\" is not a SELECT"
            )));
        };
        let body = unwrap_column_list(*body);
        if not_updatable(&body).is_some() {
            return Err(Error::Sqlstate(
                "55000",
                format!("cannot {verb} view \"{view}\""),
            ));
        }
        let kind = check_kind(&view);
        let Some(N::RangeVar(base)) = body.from_clause[0].node.clone() else {
            unreachable!("checked by not_updatable");
        };
        let qualifier = base
            .alias
            .as_ref()
            .map(|a| a.aliasname.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| base.relname.clone());
        let columns = view_columns(&body, &base.relname, lookup)?;
        let mut view_names = vec![view.clone()];
        if let Some(a) = relation.alias.as_ref() {
            view_names.push(a.aliasname.clone());
        }
        let writable = |name: &str| -> Result<String> {
            match columns.iter().find(|c| c.name == name) {
                Some(ViewColumn { base: Some(b), .. }) => Ok(b.clone()),
                Some(_) => Err(Error::Sqlstate(
                    "0A000",
                    format!("cannot {what} column \"{name}\" of view \"{view}\""),
                )),
                None => Err(Error::UndefinedColumn(name.to_string())),
            }
        };
        let view_where = body.where_clause.as_deref().cloned();
        // The conditions from the views above are over THIS view's columns;
        // carry them down to its base. Then this view's own, when it (or a
        // CASCADED view above it) checks.
        for (_, cond) in &mut checks {
            substitute(cond, &columns, &view_names, &qualifier)?;
        }
        if kind.as_deref() == Some("CASCADED") {
            cascade = true;
        }
        if kind.is_some() || cascade {
            if let Some(w) = view_where.clone() {
                checks.push((view.clone(), w));
            }
        }
        match node.node.as_mut() {
            Some(N::InsertStmt(i)) => {
                if i.cols.is_empty() {
                    // Positional: the first N view columns, N the width of
                    // what is inserted.
                    let width = match i.select_stmt.as_deref().and_then(|s| s.node.as_ref()) {
                        Some(N::SelectStmt(s)) if !s.values_lists.is_empty() => {
                            match s.values_lists[0].node.as_ref() {
                                Some(N::List(l)) => l.items.len(),
                                _ => columns.len(),
                            }
                        }
                        Some(N::SelectStmt(s)) => s.target_list.len(),
                        _ => 0,
                    };
                    for vc in columns.iter().take(width) {
                        i.cols.push(pg_query::protobuf::Node {
                            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                                name: vc.name.clone(),
                                location: -1,
                                ..Default::default()
                            }))),
                        });
                    }
                }
                apply_view_defaults(i, &view)?;
                for c in &mut i.cols {
                    if let Some(N::ResTarget(rt)) = c.node.as_mut() {
                        rt.name = writable(&rt.name)?;
                    }
                }
                for r in &mut i.returning_list {
                    if let Some(N::ResTarget(rt)) = r.node.as_mut() {
                        if let Some(v) = rt.val.as_deref_mut() {
                            if rt.name.is_empty() {
                                if let Some(N::ColumnRef(c)) = v.node.as_ref() {
                                    rt.name = ref_parts(c)
                                        .and_then(|p| p.last().cloned())
                                        .unwrap_or_default();
                                }
                            }
                            substitute(v, &columns, &view_names, &qualifier)?;
                        }
                    }
                }
                i.relation = Some(base.clone());
            }
            Some(N::UpdateStmt(u)) => {
                for t in &mut u.target_list {
                    if let Some(N::ResTarget(rt)) = t.node.as_mut() {
                        rt.name = writable(&rt.name)?;
                        if let Some(v) = rt.val.as_deref_mut() {
                            substitute(v, &columns, &view_names, &qualifier)?;
                        }
                    }
                }
                let mut own = u.where_clause.take().map(|w| *w);
                if let Some(w) = own.as_mut() {
                    substitute(w, &columns, &view_names, &qualifier)?;
                }
                u.where_clause = and(view_where, own);
                for r in &mut u.returning_list {
                    if let Some(N::ResTarget(rt)) = r.node.as_mut() {
                        if let Some(v) = rt.val.as_deref_mut() {
                            if rt.name.is_empty() {
                                if let Some(N::ColumnRef(c)) = v.node.as_ref() {
                                    rt.name = ref_parts(c)
                                        .and_then(|p| p.last().cloned())
                                        .unwrap_or_default();
                                }
                            }
                            substitute(v, &columns, &view_names, &qualifier)?;
                        }
                    }
                }
                u.relation = Some(base.clone());
            }
            Some(N::DeleteStmt(d)) => {
                let mut own = d.where_clause.take().map(|w| *w);
                if let Some(w) = own.as_mut() {
                    substitute(w, &columns, &view_names, &qualifier)?;
                }
                d.where_clause = and(view_where, own);
                for r in &mut d.returning_list {
                    if let Some(N::ResTarget(rt)) = r.node.as_mut() {
                        if let Some(v) = rt.val.as_deref_mut() {
                            if rt.name.is_empty() {
                                if let Some(N::ColumnRef(c)) = v.node.as_ref() {
                                    rt.name = ref_parts(c)
                                        .and_then(|p| p.last().cloned())
                                        .unwrap_or_default();
                                }
                            }
                            substitute(v, &columns, &view_names, &qualifier)?;
                        }
                    }
                }
                d.relation = Some(base.clone());
            }
            _ => return Ok(()),
        }
    }
    Err(Error::Sqlstate(
        "42P17",
        "infinite recursion detected in rules for a view".into(),
    ))
}
