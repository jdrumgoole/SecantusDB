//! A mutable pre-order walk over a parsed statement's nodes, for the rewrites
//! the planner applies before it looks at a tree (`collate`, `flatten`).

use pg_query::protobuf::{self as pb, node::Node as N, Node};

fn visit_opt(n: &mut Option<Box<Node>>, f: &mut dyn FnMut(&mut Node) -> bool) {
    if let Some(n) = n.as_deref_mut() {
        walk(n, f);
    }
}

fn visit_all(ns: &mut [Node], f: &mut dyn FnMut(&mut Node) -> bool) {
    for n in ns {
        walk(n, f);
    }
}

pub(crate) fn walk_select(s: &mut pb::SelectStmt, f: &mut dyn FnMut(&mut Node) -> bool) {
    visit_all(&mut s.distinct_clause, f);
    visit_all(&mut s.target_list, f);
    visit_all(&mut s.from_clause, f);
    visit_opt(&mut s.where_clause, f);
    visit_all(&mut s.group_clause, f);
    visit_opt(&mut s.having_clause, f);
    visit_all(&mut s.window_clause, f);
    visit_all(&mut s.values_lists, f);
    visit_all(&mut s.sort_clause, f);
    visit_opt(&mut s.limit_offset, f);
    visit_opt(&mut s.limit_count, f);
    if let Some(w) = s.with_clause.as_mut() {
        visit_all(&mut w.ctes, f);
    }
    if let Some(l) = s.larg.as_deref_mut() {
        walk_select(l, f);
    }
    if let Some(r) = s.rarg.as_deref_mut() {
        walk_select(r, f);
    }
}

/// Apply `f` to `node` and then to every expression and statement node
/// under it, pre-order. `f` answers `true` when it REPLACED the node, in
/// which case it is run again on the replacement before the walk descends
/// (`(a COLLATE "C") COLLATE "C"` unwraps twice).
pub(crate) fn walk(node: &mut Node, f: &mut dyn FnMut(&mut Node) -> bool) {
    while f(node) {}
    match node.node.as_mut() {
        Some(N::SelectStmt(s)) => walk_select(s, f),
        Some(N::InsertStmt(s)) => {
            visit_opt(&mut s.select_stmt, f);
            visit_all(&mut s.returning_list, f);
        }
        Some(N::UpdateStmt(s)) => {
            visit_all(&mut s.target_list, f);
            visit_opt(&mut s.where_clause, f);
            visit_all(&mut s.from_clause, f);
            visit_all(&mut s.returning_list, f);
        }
        Some(N::DeleteStmt(s)) => {
            visit_all(&mut s.using_clause, f);
            visit_opt(&mut s.where_clause, f);
            visit_all(&mut s.returning_list, f);
        }
        Some(N::ResTarget(r)) => visit_opt(&mut r.val, f),
        Some(N::AExpr(e)) => {
            visit_opt(&mut e.lexpr, f);
            visit_opt(&mut e.rexpr, f);
        }
        Some(N::BoolExpr(e)) => visit_all(&mut e.args, f),
        Some(N::FuncCall(call)) => {
            visit_all(&mut call.args, f);
            visit_all(&mut call.agg_order, f);
            visit_opt(&mut call.agg_filter, f);
        }
        Some(N::TypeCast(t)) => visit_opt(&mut t.arg, f),
        Some(N::CollateClause(c)) => visit_opt(&mut c.arg, f),
        Some(N::CaseExpr(c)) => {
            visit_opt(&mut c.arg, f);
            visit_all(&mut c.args, f);
            visit_opt(&mut c.defresult, f);
        }
        Some(N::CaseWhen(w)) => {
            visit_opt(&mut w.expr, f);
            visit_opt(&mut w.result, f);
        }
        Some(N::NullTest(t)) => visit_opt(&mut t.arg, f),
        Some(N::BooleanTest(t)) => visit_opt(&mut t.arg, f),
        Some(N::SubLink(s)) => {
            visit_opt(&mut s.testexpr, f);
            visit_opt(&mut s.subselect, f);
        }
        Some(N::RangeSubselect(r)) => visit_opt(&mut r.subquery, f),
        Some(N::JoinExpr(j)) => {
            visit_opt(&mut j.larg, f);
            visit_opt(&mut j.rarg, f);
            visit_opt(&mut j.quals, f);
        }
        Some(N::SortBy(s)) => visit_opt(&mut s.node, f),
        Some(N::CoalesceExpr(c)) => visit_all(&mut c.args, f),
        Some(N::MinMaxExpr(m)) => visit_all(&mut m.args, f),
        Some(N::AIndirection(a)) => visit_opt(&mut a.arg, f),
        Some(N::AArrayExpr(a)) => visit_all(&mut a.elements, f),
        Some(N::RowExpr(r)) => visit_all(&mut r.args, f),
        Some(N::List(l)) => visit_all(&mut l.items, f),
        Some(N::NamedArgExpr(n)) => visit_opt(&mut n.arg, f),
        Some(N::CommonTableExpr(c)) => visit_opt(&mut c.ctequery, f),
        Some(N::RangeFunction(r)) => visit_all(&mut r.functions, f),
        _ => {}
    }
}
