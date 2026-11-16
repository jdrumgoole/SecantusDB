//! `pull_up_sublinks` for an EXPLAINed SELECT (RP-411): a WHERE that is
//! one `col IN (SELECT col FROM t)` or one `EXISTS (SELECT ... FROM t
//! WHERE t.x = outer.y)` is the semi join `convert_ANY_sublink_to_join` /
//! `convert_EXISTS_sublink_to_join` make of it, planned as a `Hash Semi
//! Join` of the outer relation's scan to the inner one's, hashed.
//!
//! Recognised here: a plain parent over one plain relation whose whole
//! WHERE is that sublink, over a subquery reading one plain relation, with
//! no WHERE (IN) or a WHERE that is the one correlating column equality
//! (EXISTS). Anything else is left to the plan as written.
//!
//! Over an inner relation unique on its column, `reduce_unique_semijoins`
//! makes the semi join an inner join: `as_inner_join` writes that join, and
//! the server plans it when a unique index fixes the column.
//!
//! Otherwise only the node shape is modelled: a semi join has no planner
//! path or price here yet (`eqjoinsel_semi`, and the unique-ified inner
//! join `add_path` weighs against it), so its costs print as zeros.

use super::*;
use pg_query::protobuf::{ColumnRef, RangeVar, SelectStmt, SubLinkType};

/// A semi join's sides and its one hash clause, as EXPLAIN prints them.
#[derive(Debug, Clone, PartialEq)]
pub struct SemiJoin {
    /// The outer relation's table and the name its scan prints (its alias).
    pub outer: (String, String),
    /// The inner relation's table and printed name.
    pub inner: (String, String),
    /// The inner relation's column the clause compares.
    pub inner_col: String,
    /// The outer relation's column the clause compares.
    pub outer_col: String,
    /// `(outer.col = inner.col)`.
    pub hash_cond: String,
    /// The EXPLAIN of the inner join `reduce_unique_semijoins` makes of
    /// it when the inner relation is unique on `inner_col` (RP-411):
    /// `outer JOIN inner ON outer.col = inner.col`, the select list read
    /// from the outer side. Planned in `plan_explain`; the server, which
    /// knows the unique indexes, chooses it.
    pub inner_join: Option<Box<Statement>>,
}

fn plain(s: &SelectStmt) -> bool {
    s.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && s.with_clause.is_none()
        && s.group_clause.is_empty()
        && s.having_clause.is_none()
        && s.distinct_clause.is_empty()
        && s.limit_count.is_none()
        && s.limit_offset.is_none()
        && s.sort_clause.is_empty()
        && s.window_clause.is_empty()
        && s.locking_clause.is_empty()
        && s.into_clause.is_none()
        && s.values_lists.is_empty()
}

fn one_relation(s: &SelectStmt) -> Option<&RangeVar> {
    let [item] = s.from_clause.as_slice() else {
        return None;
    };
    match item.node.as_ref() {
        Some(N::RangeVar(rv))
            if rv.inh
                && rv.schemaname.is_empty()
                && rv.alias.as_ref().is_none_or(|a| a.colnames.is_empty()) =>
        {
            Some(rv)
        }
        _ => None,
    }
}

fn shown_name(rv: &RangeVar) -> String {
    rv.alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_else(|| rv.relname.clone())
}

fn column_parts(n: &pg_query::protobuf::Node) -> Option<Vec<String>> {
    let Some(N::ColumnRef(ColumnRef { fields, .. })) = n.node.as_ref() else {
        return None;
    };
    fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A column reference resolved against `sides` (`(printed name, table)`):
/// `(side index, column)`. An unqualified name is the first side's that has
/// it, as the subquery's own relation shadows the outer one.
fn resolve(n: &pg_query::protobuf::Node, sides: &[(&str, &TableDef)]) -> Option<(usize, String)> {
    let parts = column_parts(n)?;
    match parts.as_slice() {
        [col] => {
            let mut hits = sides
                .iter()
                .enumerate()
                .filter(|(_, (_, d))| user_column(d, col).is_some());
            let (i, _) = hits.next()?;
            Some((i, col.clone()))
        }
        [rel, col] => {
            let i = sides.iter().position(|(name, _)| name == rel)?;
            user_column(sides[i].1, col)?;
            Some((i, col.clone()))
        }
        _ => None,
    }
}

fn user_column<'a>(def: &'a TableDef, col: &str) -> Option<&'a Column> {
    def.columns.iter().find(|c| c.name == col)
}

fn is_equals(names: &[pg_query::protobuf::Node]) -> bool {
    matches!(
        names,
        [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "=")
    )
}

/// The semi join `query`'s WHERE pulls up into, if it is one.
pub(crate) fn semi_join_of(
    query: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<Box<SemiJoin>> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    if !plain(s) {
        return None;
    }
    let outer_rv = one_relation(s)?;
    let outer_def = lookup(&outer_rv.relname)?;
    let outer_name = shown_name(outer_rv);
    let Some(N::SubLink(link)) = s.where_clause.as_deref()?.node.as_ref() else {
        return None;
    };
    let Some(N::SelectStmt(sub)) = link.subselect.as_deref()?.node.as_ref() else {
        return None;
    };
    if !plain(sub) {
        return None;
    }
    let inner_rv = one_relation(sub)?;
    let inner_def = lookup(&inner_rv.relname)?;
    let inner_name = shown_name(inner_rv);
    if inner_name == outer_name {
        return None;
    }
    // Inner first: within the subquery its own relation shadows the outer.
    let sides = [
        (inner_name.as_str(), &inner_def),
        (outer_name.as_str(), &outer_def),
    ];
    let (outer_col, inner_col) = match SubLinkType::try_from(link.sub_link_type).ok()? {
        SubLinkType::AnySublink => {
            // `IN` leaves the operator name empty; `= ANY` names it.
            if !(link.oper_name.is_empty() || is_equals(&link.oper_name))
                || sub.where_clause.is_some()
            {
                return None;
            }
            let [t] = sub.target_list.as_slice() else {
                return None;
            };
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                return None;
            };
            let (0, inner_col) = resolve(rt.val.as_deref()?, &sides[..1])? else {
                return None;
            };
            let outer = resolve(link.testexpr.as_deref()?, &[sides[1]])?.1;
            (outer, inner_col)
        }
        SubLinkType::ExistsSublink => {
            let Some(N::AExpr(e)) = sub.where_clause.as_deref()?.node.as_ref() else {
                return None;
            };
            if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !is_equals(&e.name) {
                return None;
            }
            let l = resolve(e.lexpr.as_deref()?, &sides)?;
            let r = resolve(e.rexpr.as_deref()?, &sides)?;
            match (l, r) {
                ((0, i), (1, o)) | ((1, o), (0, i)) => (o, i),
                _ => return None,
            }
        }
        _ => return None,
    };
    // Two types would print the coercion PostgreSQL adds; not modelled.
    if user_column(&outer_def, &outer_col)?.pg_type != user_column(&inner_def, &inner_col)?.pg_type
    {
        return None;
    }
    Some(Box::new(SemiJoin {
        hash_cond: format!("({outer_name}.{outer_col} = {inner_name}.{inner_col})"),
        outer: (outer_rv.relname.clone(), outer_name),
        inner: (inner_rv.relname.clone(), inner_name),
        inner_col,
        outer_col,
        inner_join: None,
    }))
}

fn column_ref(fields: Vec<N>) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(ColumnRef {
            fields: fields
                .into_iter()
                .map(|f| pg_query::protobuf::Node { node: Some(f) })
                .collect(),
            location: -1,
        })),
    }
}

fn string(s: &str) -> N {
    N::String(pg_query::protobuf::String { sval: s.into() })
}

/// `query` with its WHERE sublink `sj` made the inner join
/// `reduce_unique_semijoins` leaves of a semi join whose inner side is
/// unique (RP-411): FROM both relations, the hash clause as its WHERE, and
/// each select-list column read from the outer side. `None` when the select
/// list has anything but `*` and the outer side's own columns.
pub(crate) fn as_inner_join(query: &N, sj: &SemiJoin) -> Option<N> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    let Some(N::SubLink(link)) = s.where_clause.as_deref()?.node.as_ref() else {
        return None;
    };
    let Some(N::SelectStmt(sub)) = link.subselect.as_deref()?.node.as_ref() else {
        return None;
    };
    let inner_rv = one_relation(sub)?.clone();
    let outer = sj.outer.1.as_str();
    let mut out = s.clone();
    for t in &mut out.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_mut() else {
            return None;
        };
        let parts = rt.val.as_deref().and_then(|v| match v.node.as_ref() {
            Some(N::ColumnRef(c)) => Some(c.fields.clone()),
            _ => None,
        })?;
        let qualified = match parts.as_slice() {
            [f] if matches!(f.node, Some(N::AStar(_))) => {
                vec![string(outer), N::AStar(pg_query::protobuf::AStar {})]
            }
            [f] => match f.node.as_ref() {
                Some(N::String(c)) => vec![string(outer), string(&c.sval)],
                _ => return None,
            },
            [rel, _] if matches!(rel.node.as_ref(), Some(N::String(r)) if r.sval == outer) => {
                parts.iter().filter_map(|p| p.node.clone()).collect()
            }
            _ => return None,
        };
        rt.val = Some(Box::new(column_ref(qualified)));
    }
    out.from_clause.push(pg_query::protobuf::Node {
        node: Some(N::RangeVar(inner_rv)),
    });
    let side = |r: &str, c: &str| Box::new(column_ref(vec![string(r), string(c)]));
    out.where_clause = Some(Box::new(pg_query::protobuf::Node {
        node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: pg_query::protobuf::AExprKind::AexprOp as i32,
            name: vec![pg_query::protobuf::Node {
                node: Some(string("=")),
            }],
            lexpr: Some(side(outer, &sj.outer_col)),
            rexpr: Some(side(&sj.inner.1, &sj.inner_col)),
            location: -1,
        }))),
    }));
    Some(N::SelectStmt(out))
}
