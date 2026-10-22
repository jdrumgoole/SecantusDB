//! Field selection on an anonymous record read from a FROM subquery or a
//! CTE: `(r).f1 FROM (SELECT row(1, 'a') AS r) s`.
//!
//! PostgreSQL names a `ROW()`'s fields `f1`, `f2`, ... only where it can see
//! the `ROW()`; for a subquery or CTE column it looks through to the
//! column's expression (`expandRecordVariable`), so the field has the type
//! of the expression it came from -- `(r).f1` above is an integer, and a
//! nested `ROW()` field is a record. Here the column reaches the outer query
//! as a value of type `record`, so its fields are typed by what they hold
//! (and described as text before a row is seen); this pass makes the type
//! the planner already knows explicit, as a cast of the selection.
//!
//! The same look-through decides what PostgreSQL refuses (measured on
//! 15.19):
//! - a field of a field that is itself an anonymous record,
//!   `((r).f1).f2`, is 42703 `could not identify column "f2" in record data
//!   type`, at the record;
//! - so is a field of a scalar subquery's ROW(), `((select row(1,
//!   'a'))).f1`, at the subquery;
//! - a field of a VALUES column holding records, `(x).f1 FROM (VALUES
//!   (row(1, 'a'))) v(x)`, is 42809 `record type has not been registered`,
//!   as is `(x).*` there and `((select row(1, 'a'))).*`.

use super::*;

/// Where a FROM item's record column comes from.
enum Origin {
    /// A `ROW(...)` in the select list of `leaf`, whose FROM types its
    /// arguments.
    Row(
        Vec<pg_query::protobuf::Node>,
        Box<pg_query::protobuf::SelectStmt>,
    ),
    /// A VALUES list column of records.
    Values,
    /// `information_schema._pg_expandarray(arr)` in the select list of
    /// `leaf`: its OUT parameters, `x` (the array's element) and `n integer`.
    Expand(
        Box<pg_query::protobuf::Node>,
        Box<pg_query::protobuf::SelectStmt>,
    ),
}

/// `(alias, column, origin)` for every record column the FROM items expose.
type Origins = Vec<(String, String, Origin)>;

/// The leftmost arm of a set operation (a plain SELECT is its own).
fn leftmost(s: &pg_query::protobuf::SelectStmt) -> &pg_query::protobuf::SelectStmt {
    match s.larg.as_deref() {
        Some(l) if s.op != pg_query::protobuf::SetOperation::SetopNone as i32 => leftmost(l),
        _ => s,
    }
}

fn names(list: &[pg_query::protobuf::Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn add_origins(
    alias: &str,
    colnames: &[String],
    q: &pg_query::protobuf::SelectStmt,
    out: &mut Origins,
) {
    let leaf = leftmost(q);
    if let Some(Some(N::List(first))) = leaf.values_lists.first().map(|r| r.node.as_ref()) {
        for (k, item) in first.items.iter().enumerate() {
            if matches!(item.node.as_ref(), Some(N::RowExpr(_))) {
                let name = colnames
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| format!("column{}", k + 1));
                out.push((alias.to_string(), name, Origin::Values));
            }
        }
        return;
    }
    for (i, t) in leaf.target_list.iter().enumerate() {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            continue;
        };
        let val = rt.val.as_deref().and_then(|v| v.node.as_ref());
        if let Some(N::FuncCall(fc)) = val {
            if let ([arg], Some("_pg_expandarray")) = (fc.args.as_slice(), func_name(fc).as_deref())
            {
                let name = colnames.get(i).cloned().unwrap_or_else(|| {
                    if rt.name.is_empty() {
                        "_pg_expandarray".to_string()
                    } else {
                        rt.name.clone()
                    }
                });
                out.push((
                    alias.to_string(),
                    name,
                    Origin::Expand(Box::new(arg.clone()), Box::new(leaf.clone())),
                ));
            }
            continue;
        }
        let Some(N::RowExpr(row)) = val else {
            continue;
        };
        let name = colnames.get(i).cloned().unwrap_or_else(|| {
            if rt.name.is_empty() {
                "row".to_string()
            } else {
                rt.name.clone()
            }
        });
        out.push((
            alias.to_string(),
            name,
            Origin::Row(row.args.clone(), Box::new(leaf.clone())),
        ));
    }
}

fn origins(s: &pg_query::protobuf::SelectStmt) -> Origins {
    let mut out = Vec::new();
    let ctes: Vec<&pg_query::protobuf::CommonTableExpr> = s
        .with_clause
        .iter()
        .filter(|w| !w.recursive)
        .flat_map(|w| w.ctes.iter())
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(c.as_ref()),
            _ => None,
        })
        .collect();
    for item in &s.from_clause {
        match item.node.as_ref() {
            Some(N::RangeSubselect(rs)) if !rs.lateral => {
                let Some(alias) = rs.alias.as_ref() else {
                    continue;
                };
                if let Some(N::SelectStmt(q)) = rs.subquery.as_deref().and_then(|n| n.node.as_ref())
                {
                    add_origins(&alias.aliasname, &names(&alias.colnames), q, &mut out);
                }
            }
            Some(N::RangeVar(r)) if r.schemaname.is_empty() => {
                let Some(cte) = ctes.iter().rev().find(|c| c.ctename == r.relname) else {
                    continue;
                };
                let Some(N::SelectStmt(q)) = cte.ctequery.as_deref().and_then(|n| n.node.as_ref())
                else {
                    continue;
                };
                let alias = r
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| r.relname.clone());
                let mut cols = names(&cte.aliascolnames);
                if let Some(a) = r.alias.as_ref().filter(|a| !a.colnames.is_empty()) {
                    cols = names(&a.colnames);
                }
                add_origins(&alias, &cols, q, &mut out);
            }
            _ => {}
        }
    }
    out
}

/// The origin a column reference names: `r` when exactly one record
/// column is called that and no other FROM item could have it, or `s.r`.
fn resolve<'o>(
    c: &pg_query::protobuf::ColumnRef,
    s: &pg_query::protobuf::SelectStmt,
    origins: &'o Origins,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<&'o Origin> {
    let parts = names(&c.fields);
    if parts.len() != c.fields.len() {
        return None;
    }
    match parts.as_slice() {
        [col] => {
            let mut hits = origins.iter().filter(|(_, n, _)| n == col);
            let hit = hits.next()?;
            if hits.next().is_some() {
                return None;
            }
            // Another FROM item (a table) with the same column makes the name
            // ambiguous, which is not this pass's to answer.
            let elsewhere = s.from_clause.iter().any(|f| match f.node.as_ref() {
                Some(N::RangeVar(r)) => lookup(&relation_name(r))
                    .is_some_and(|d| d.column(col).is_some() && r.relname != hit.0),
                Some(N::RangeSubselect(_)) => false,
                _ => true,
            });
            (!elsewhere).then_some(&hit.2)
        }
        [q, col] => origins
            .iter()
            .find(|(a, n, _)| a == q && n == col)
            .map(|(_, _, o)| o),
        _ => None,
    }
}

/// `fN` as a 0-based position.
fn position(field: &str) -> Option<usize> {
    field
        .strip_prefix('f')
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .map(|n| n - 1)
}

/// The single field name of `(arg).field`, and its argument.
fn selection(n: &pg_query::protobuf::Node) -> Option<(&pg_query::protobuf::Node, &str)> {
    let Some(N::AIndirection(ind)) = n.node.as_ref() else {
        return None;
    };
    let [f] = ind.indirection.as_slice() else {
        return None;
    };
    let Some(N::String(f)) = f.node.as_ref() else {
        return None;
    };
    Some((ind.arg.as_deref()?, f.sval.as_str()))
}

/// `(arg).*` -- its argument.
fn star_selection(n: &pg_query::protobuf::Node) -> Option<&pg_query::protobuf::Node> {
    let Some(N::AIndirection(ind)) = n.node.as_ref() else {
        return None;
    };
    let [f] = ind.indirection.as_slice() else {
        return None;
    };
    matches!(f.node.as_ref(), Some(N::AStar(_))).then_some(ind.arg.as_deref()?)
}

/// Is `n` a scalar subquery whose value is a ROW() (an anonymous record)?
fn row_sublink(n: &pg_query::protobuf::Node) -> bool {
    let Some(N::SubLink(sl)) = n.node.as_ref() else {
        return false;
    };
    if sl.sub_link_type != pg_query::protobuf::SubLinkType::ExprSublink as i32 {
        return false;
    }
    let Some(N::SelectStmt(q)) = sl.subselect.as_deref().and_then(|q| q.node.as_ref()) else {
        return false;
    };
    let leaf = leftmost(q);
    matches!(
        leaf.target_list.as_slice(),
        [t] if matches!(t.node.as_ref(), Some(N::ResTarget(rt))
            if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()), Some(N::RowExpr(_))))
    )
}

/// `((select row(...))).f1` is 42703 at the subquery and `((select
/// row(...))).*` 42809: a scalar subquery's record has no field names.
pub(crate) fn check_sublink_selection(n: &pg_query::protobuf::Node) -> Result<()> {
    if star_selection(n).is_some_and(row_sublink) {
        return Err(Error::Sqlstate(
            "42809",
            "record type has not been registered".into(),
        ));
    }
    if let Some((arg, field)) = selection(n).filter(|(a, _)| row_sublink(a)) {
        if let Some(loc) = expr_location(arg) {
            set_error_location(loc);
        }
        return Err(Error::UndefinedField(format!(
            "could not identify column \"{field}\" in record data type"
        )));
    }
    Ok(())
}

/// The ROW() argument `(c).field` selects, when `c` is a record column
/// with a visible origin.
fn selected_arg<'o>(
    arg: &pg_query::protobuf::Node,
    field: &str,
    s: &pg_query::protobuf::SelectStmt,
    origins: &'o Origins,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<(
    &'o pg_query::protobuf::Node,
    &'o pg_query::protobuf::SelectStmt,
)> {
    let Some(N::ColumnRef(c)) = arg.node.as_ref() else {
        return None;
    };
    match resolve(c, s, origins, lookup)? {
        Origin::Row(args, leaf) => Some((args.get(position(field)?)?, leaf)),
        Origin::Values | Origin::Expand(..) => None,
    }
}

/// The type of `(c).x` / `(c).n` where `c` is a subquery's
/// `_pg_expandarray(arr)` column.
fn expand_field_type(
    arg: &pg_query::protobuf::Node,
    field: &str,
    s: &pg_query::protobuf::SelectStmt,
    origins: &Origins,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let Some(N::ColumnRef(c)) = arg.node.as_ref() else {
        return None;
    };
    let Origin::Expand(array, leaf) = resolve(c, s, origins, lookup)? else {
        return None;
    };
    match field {
        "n" => Some("int4".to_string()),
        "x" => element_type(array, leaf, lookup),
        _ => None,
    }
}

/// The element type of the array expression `e` in `leaf`'s select list.
fn element_type(
    e: &pg_query::protobuf::Node,
    leaf: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let of_array = |t: &str| match t {
        "int2vector" => Some("int2".to_string()),
        "oidvector" => Some("oid".to_string()),
        _ => t.strip_suffix("[]").map(str::to_string),
    };
    match e.node.as_ref()? {
        N::TypeCast(tc) => of_array(&type_name_of(tc.type_name.as_ref()?)),
        N::AArrayExpr(a) => optype::type_in_select(leaf, a.elements.first()?, lookup),
        N::CoalesceExpr(c) => c.args.iter().find_map(|a| element_type(a, leaf, lookup)),
        _ => of_array(&optype::type_in_select(leaf, e, lookup)?),
    }
}

/// The type the selected field has, when the planner can name it.
fn field_type(
    e: &pg_query::protobuf::Node,
    leaf: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    if matches!(e.node.as_ref(), Some(N::RowExpr(_))) {
        return Some("record".to_string());
    }
    optype::type_in_select(leaf, e, lookup)
}

/// Rewrite `s`'s field selections on subquery / CTE record columns into
/// casts to their fields' types, refusing what PostgreSQL refuses; `None`
/// when nothing changes.
pub(crate) fn rewrite(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let origins = origins(s);
    let mut out = s.clone();
    let mut changed = false;
    // The selection a cast (ours from an earlier pass, or the user's) already
    // wraps is the next node visited; it is left as it is.
    let mut skip_next = false;
    let mut visit = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        if std::mem::take(&mut skip_next) {
            return Ok(());
        }
        if let Some(N::TypeCast(tc)) = n.node.as_ref() {
            if let Some((arg, field)) = tc.arg.as_deref().and_then(selection) {
                skip_next = selected_arg(arg, field, s, &origins, lookup).is_some()
                    || expand_field_type(arg, field, s, &origins, lookup).is_some();
            }
            return Ok(());
        }
        if let Some(arg) = star_selection(n) {
            let values = matches!(arg.node.as_ref(), Some(N::ColumnRef(c))
                if matches!(resolve(c, s, &origins, lookup), Some(Origin::Values)));
            if values {
                return Err(Error::Sqlstate(
                    "42809",
                    "record type has not been registered".into(),
                ));
            }
            return Ok(());
        }
        let Some((arg, field)) = selection(n) else {
            return Ok(());
        };
        // `((r).f1).f2` over a record field: the inner selection is an
        // anonymous record PostgreSQL cannot see into.
        if let Some((inner, inner_field)) = selection(arg) {
            if let Some((e, _)) = selected_arg(inner, inner_field, s, &origins, lookup) {
                if matches!(e.node.as_ref(), Some(N::RowExpr(_))) {
                    if let Some(loc) = expr_location(inner) {
                        set_error_location(loc);
                    }
                    return Err(Error::UndefinedField(format!(
                        "could not identify column \"{field}\" in record data type"
                    )));
                }
            }
            return Ok(());
        }
        if let Some(N::ColumnRef(c)) = arg.node.as_ref() {
            if let Some(Origin::Values) = resolve(c, s, &origins, lookup) {
                return Err(Error::Sqlstate(
                    "42809",
                    "record type has not been registered".into(),
                ));
            }
        }
        let ty = match selected_arg(arg, field, s, &origins, lookup) {
            Some((e, leaf)) => field_type(e, leaf, lookup),
            None => expand_field_type(arg, field, s, &origins, lookup),
        };
        let Some(ty) = ty else {
            return Ok(());
        };
        let selected = n.clone();
        *n = pg_query::protobuf::Node {
            node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                arg: Some(Box::new(selected)),
                type_name: Some(type_name_node(&ty)),
                location: -1,
            }))),
        };
        changed = true;
        skip_next = true;
        Ok(())
    };
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                walk_expr(v, &mut visit)?;
            }
        }
    }
    if let Some(w) = out.where_clause.as_deref_mut() {
        walk_expr(w, &mut visit)?;
    }
    for sb in &mut out.sort_clause {
        if let Some(N::SortBy(b)) = sb.node.as_mut() {
            if let Some(n) = b.node.as_deref_mut() {
                walk_expr(n, &mut visit)?;
            }
        }
    }
    Ok(changed.then_some(out))
}

/// `(ss.x).n` / `(ss.x).x` over a FROM subquery's `_pg_expandarray(arr)`
/// column, as a column of the subquery itself: the subquery selects
/// `(_pg_expandarray(arr)).n` beside `x`, and the outer query reads that
/// column. The value is the same; what changes is that a join or filter on
/// it compares a column, which the join planner can hash, where the field
/// selection made every pair of rows a candidate -- `information_schema
/// .parameters` joins `pg_type` on `t.oid = (ss.x).x` over every function's
/// arguments. Only where the extra column cannot show: no `*` in the outer
/// select list, and a plain subquery (no set operation, DISTINCT, GROUP BY
/// or window) that it can be added to.
pub(crate) fn push_down(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let star = s.target_list.iter().any(|t| {
        t.node.as_ref().is_some_and(|n| {
            n.nodes()
                .iter()
                .any(|(n, _, _, _)| matches!(n, pg_query::NodeRef::AStar(_)))
        })
    });
    if star {
        return Ok(None);
    }
    let origins = origins(s);
    if !origins
        .iter()
        .any(|(_, _, o)| matches!(o, Origin::Expand(..)))
    {
        return Ok(None);
    }
    // (alias, column, field, the subquery column standing in for it, the
    // type the selection is cast to)
    let mut wanted: Vec<(
        String,
        String,
        String,
        String,
        Option<pg_query::protobuf::TypeName>,
    )> = Vec::new();
    let mut out = s.clone();
    let mut visit = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        // The cast `rewrite` put around the selection goes with it into the
        // subquery, so the outer query reads a bare column (a cast key would
        // not hash either).
        let (selected, cast) = match n.node.as_ref() {
            Some(N::TypeCast(tc)) => match tc.arg.as_deref() {
                Some(a) if selection(a).is_some() => (a.clone(), tc.type_name.clone()),
                _ => return Ok(()),
            },
            _ => (n.clone(), None),
        };
        let Some((arg, field)) = selection(&selected) else {
            return Ok(());
        };
        if !matches!(field, "x" | "n") {
            return Ok(());
        }
        let Some(N::ColumnRef(c)) = arg.node.as_ref() else {
            return Ok(());
        };
        if !matches!(resolve(c, s, &origins, lookup), Some(Origin::Expand(..))) {
            return Ok(());
        }
        let parts = names(&c.fields);
        let (alias, column) = match parts.as_slice() {
            [q, col] => (q.clone(), col.clone()),
            [col] => match origins.iter().find(|(_, n, _)| n == col) {
                Some((a, _, _)) => (a.clone(), col.clone()),
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        let cast_name: String = cast
            .as_ref()
            .map(type_name_of)
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let name = format!("__pgx_{column}_{field}_{cast_name}");
        if !wanted.iter().any(|w| w.0 == alias && w.3 == name) {
            wanted.push((alias.clone(), column, field.to_string(), name.clone(), cast));
        }
        *n = pg_query::protobuf::Node {
            node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                fields: vec![string_node(&alias), string_node(&name)],
                location: -1,
            })),
        };
        Ok(())
    };
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            // An unnamed `(ss.x).n` is the column `n`, as PostgreSQL names a
            // field selection; the column standing in for it must keep that.
            if rt.name.is_empty() {
                let field = rt.val.as_deref().and_then(|v| {
                    let v = match v.node.as_ref() {
                        Some(N::TypeCast(tc)) => tc.arg.as_deref()?,
                        _ => v,
                    };
                    selection(v).map(|(_, f)| f.to_string())
                });
                if let Some(f) = field {
                    rt.name = f;
                }
            }
            if let Some(v) = rt.val.as_deref_mut() {
                walk_expr(v, &mut visit)?;
            }
        }
    }
    if let Some(w) = out.where_clause.as_deref_mut() {
        walk_expr(w, &mut visit)?;
    }
    for sb in &mut out.sort_clause {
        if let Some(N::SortBy(b)) = sb.node.as_mut() {
            if let Some(n) = b.node.as_deref_mut() {
                walk_expr(n, &mut visit)?;
            }
        }
    }
    if wanted.is_empty() {
        return Ok(None);
    }
    for item in &mut out.from_clause {
        let Some(N::RangeSubselect(rs)) = item.node.as_mut() else {
            continue;
        };
        let Some(alias) = rs.alias.as_mut() else {
            continue;
        };
        let mine: Vec<_> = wanted.iter().filter(|w| w.0 == alias.aliasname).collect();
        if mine.is_empty() {
            continue;
        }
        let colnames = names(&alias.colnames);
        let Some(N::SelectStmt(q)) = rs.subquery.as_deref_mut().and_then(|n| n.node.as_mut())
        else {
            return Ok(None);
        };
        if q.op != pg_query::protobuf::SetOperation::SetopNone as i32
            || !q.distinct_clause.is_empty()
            || !q.group_clause.is_empty()
            || !q.window_clause.is_empty()
            || q.having_clause.is_some()
        {
            return Ok(None);
        }
        for (_, column, field, name, cast) in mine {
            // The target the column names: by the alias's list, else its own.
            let index = match colnames.iter().position(|c| c == column) {
                Some(i) => Some(i),
                None => q.target_list.iter().position(
                    |t| matches!(t.node.as_ref(), Some(N::ResTarget(rt)) if rt.name == *column),
                ),
            };
            let Some(Some(N::ResTarget(rt))) = index
                .and_then(|i| q.target_list.get(i))
                .map(|t| t.node.as_ref())
            else {
                return Ok(None);
            };
            let Some(val) = rt.val.clone() else {
                return Ok(None);
            };
            let mut selected = pg_query::protobuf::Node {
                node: Some(N::AIndirection(Box::new(
                    pg_query::protobuf::AIndirection {
                        arg: Some(val),
                        indirection: vec![string_node(field)],
                    },
                ))),
            };
            if let Some(t) = cast {
                selected = pg_query::protobuf::Node {
                    node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                        arg: Some(Box::new(selected)),
                        type_name: Some(t.clone()),
                        location: -1,
                    }))),
                };
            }
            q.target_list.push(pg_query::protobuf::Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    name: name.clone(),
                    val: Some(Box::new(selected)),
                    location: -1,
                    ..Default::default()
                }))),
            });
            if !alias.colnames.is_empty() {
                alias.colnames.push(string_node(name));
            }
        }
    }
    Ok(Some(out))
}
