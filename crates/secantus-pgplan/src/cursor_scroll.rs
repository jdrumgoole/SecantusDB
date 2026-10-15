//! Whether a cursor declared with neither `SCROLL` nor `NO SCROLL` may scan
//! backward. PostgreSQL adds `CURSOR_OPT_SCROLL` when
//! `ExecSupportsBackwardScan` holds for the plan (portalcmds.c): a scan, a
//! `VALUES` list, a function scan, a CTE scan, a subquery scan of one of
//! those, an `Append` of them, `Sort`, `Material` and `Limit` can; a join, an
//! aggregate, a window, `Unique`, `ProjectSet`, `LockRows`, a sample scan
//! and a `Result` with no input cannot. Read here from the statement's
//! shape, as measured on 15.19 (`select * from t` scrolls, `select 1`,
//! `select count(*) from t`, `select distinct ...` and a join do not).
//!
//! Where an ORDER BY decides it, it is the planner's choice of where the
//! sort goes: a Sort on top of the plan scrolls. That is clear for an
//! aggregate, an ORDER BY over a GROUP BY's other keys and a set-returning
//! target list; over a join it depends on the join method, and a join is
//! taken to be sorted on top.

use pg_query::protobuf::{self, SelectStmt, SubLinkType};
use pg_query::NodeEnum as N;

/// May a cursor over `sel` declared without a scroll option scan backward?
pub fn scrolls_by_default(sel: &SelectStmt) -> bool {
    supports_backward(sel, &[])
}

fn supports_backward(sel: &SelectStmt, outer_ctes: &[&protobuf::CommonTableExpr]) -> bool {
    if !sel.locking_clause.is_empty() {
        return false;
    }
    let mut ctes: Vec<&protobuf::CommonTableExpr> = outer_ctes.to_vec();
    if let Some(w) = sel.with_clause.as_ref() {
        for c in &w.ctes {
            if let Some(N::CommonTableExpr(c)) = c.node.as_ref() {
                ctes.push(c);
            }
        }
    }
    if sel.op != protobuf::SetOperation::SetopNone as i32 {
        // UNION ALL is an Append; the others hash or sort and de-duplicate.
        let base = sel.op == protobuf::SetOperation::SetopUnion as i32
            && sel.all
            && [&sel.larg, &sel.rarg]
                .iter()
                .all(|a| a.as_ref().is_some_and(|s| supports_backward(s, &ctes)));
        return base || !sel.sort_clause.is_empty();
    }
    if !sel.values_lists.is_empty() {
        return true;
    }
    let targets = TargetShape::of(sel);
    let sorted = !sel.sort_clause.is_empty();
    if !sel.distinct_clause.is_empty() || targets.window {
        // Unique and WindowAgg sit above any sort the ORDER BY needs.
        return false;
    }
    if !sel.group_clause.is_empty() {
        return sorted && !order_follows_grouping(sel);
    }
    if targets.aggregate || sel.having_clause.is_some() {
        return sorted;
    }
    if targets.set_returning {
        return sorted;
    }
    let from = match sel.from_clause.as_slice() {
        [] => return false,
        [one] => one,
        _ => return sorted,
    };
    let scan = match from.node.as_ref() {
        Some(N::RangeVar(r)) => match ctes
            .iter()
            .find(|c| c.ctename == r.relname && r.schemaname.is_empty())
        {
            // A CTE referenced as a relation is inlined (a subquery) unless
            // it is MATERIALIZED, which a CteScan reads.
            Some(c) => {
                c.ctematerialized == protobuf::CteMaterialize::Always as i32
                    || match c.ctequery.as_ref().and_then(|q| q.node.as_ref()) {
                        Some(N::SelectStmt(q)) => supports_backward(q, &[]),
                        _ => true,
                    }
            }
            None => true,
        },
        Some(N::RangeFunction(_)) => true,
        Some(N::RangeSubselect(s)) => match s.subquery.as_ref().and_then(|q| q.node.as_ref()) {
            Some(N::SelectStmt(q)) => supports_backward(q, &ctes),
            _ => false,
        },
        Some(N::JoinExpr(_)) => return sorted,
        _ => false,
    };
    if !scan {
        return sorted;
    }
    // An IN / EXISTS / NOT EXISTS conjunct of WHERE becomes a semi- or
    // anti-join.
    if sel.where_clause.as_deref().is_some_and(has_join_sublink) {
        return sorted;
    }
    true
}

/// Is the first ORDER BY key a GROUP BY key, so a group aggregate over the
/// sorted input delivers the order (and no Sort goes on top)?
fn order_follows_grouping(sel: &SelectStmt) -> bool {
    let Some(N::SortBy(first)) = sel.sort_clause.first().and_then(|n| n.node.as_ref()) else {
        return false;
    };
    let Some(key) = first.node.as_deref() else {
        return false;
    };
    let key = resolve_position(sel, key).unwrap_or(key);
    sel.group_clause.iter().any(|g| {
        let g = resolve_position(sel, g).unwrap_or(g);
        crate::node_print(g) == crate::node_print(key)
    })
}

/// `ORDER BY 2` / `GROUP BY 1`: the target the position names.
fn resolve_position<'a>(sel: &'a SelectStmt, n: &'a protobuf::Node) -> Option<&'a protobuf::Node> {
    let Some(N::AConst(c)) = n.node.as_ref() else {
        return None;
    };
    let Some(protobuf::a_const::Val::Ival(i)) = c.val.as_ref() else {
        return None;
    };
    let idx = usize::try_from(i.ival).ok()?.checked_sub(1)?;
    match sel.target_list.get(idx)?.node.as_ref()? {
        N::ResTarget(r) => r.val.as_deref(),
        _ => None,
    }
}

fn has_join_sublink(n: &protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::SubLink(s)) => matches!(
            SubLinkType::try_from(s.sub_link_type),
            Ok(SubLinkType::ExistsSublink | SubLinkType::AnySublink)
        ),
        Some(N::BoolExpr(b)) => match protobuf::BoolExprType::try_from(b.boolop) {
            Ok(protobuf::BoolExprType::AndExpr) => b.args.iter().any(has_join_sublink),
            Ok(protobuf::BoolExprType::NotExpr) => b.args.first().is_some_and(|a| {
                matches!(a.node.as_ref(), Some(N::SubLink(s))
                    if SubLinkType::try_from(s.sub_link_type) == Ok(SubLinkType::ExistsSublink))
            }),
            _ => false,
        },
        _ => false,
    }
}

/// What the target list computes, outside any subquery.
#[derive(Default)]
struct TargetShape {
    aggregate: bool,
    window: bool,
    set_returning: bool,
}

impl TargetShape {
    fn of(sel: &SelectStmt) -> Self {
        let mut shape = TargetShape::default();
        for t in &sel.target_list {
            let Some(mut t) = t.node.clone() else {
                continue;
            };
            // A subquery's own calls are not this level's: take every
            // SubLink's query out first (one at a time, since the walk's
            // pointers do not survive the edit).
            loop {
                let mut cut = false;
                // SAFETY: the pointers point into `t`, which outlives the walk;
                // the walk stops at the first edit.
                unsafe {
                    for (n, _, _) in t.nodes_mut() {
                        if let pg_query::NodeMut::SubLink(s) = n {
                            if (*s).subselect.is_some() {
                                (*s).subselect = None;
                                cut = true;
                                break;
                            }
                        }
                    }
                }
                if !cut {
                    break;
                }
            }
            for (n, _, _, _) in t.nodes() {
                if let pg_query::NodeRef::FuncCall(f) = n {
                    if f.over.is_some() {
                        shape.window = true;
                    } else if crate::is_aggregate_call(f) {
                        shape.aggregate = true;
                    } else if crate::func_name(f)
                        .is_some_and(|name| crate::prune_outputs::returns_set(&name))
                    {
                        shape.set_returning = true;
                    }
                }
            }
        }
        shape
    }
}
