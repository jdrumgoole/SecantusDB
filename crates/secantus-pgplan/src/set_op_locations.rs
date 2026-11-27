//! Where each column of a set operation's leaf side is, for the error
//! `transformSetOperationTree` raises when two sides' column types do not
//! match (RP-1087).
//!
//! `select_common_type` points that 42804 at `exprLocation` of the
//! right-hand column of the first pair that does not match. A leaf side's
//! column is its analysed target expression; a side that is itself a set
//! operation contributes the location of the expression ITS common type was
//! taken from, which only the executor (which knows the types) can tell, so
//! it is not computed here.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{Node, SelectStmt, SetOperation};

use crate::Statement;

/// The location of each output column of one leaf side of a set operation,
/// by position: `Some` byte offset into the statement, `Some(-1)` where
/// PostgreSQL has none (a `VALUES` side's columns are Vars made with no
/// location), `None` where it is not known here (an expression this does
/// not locate). Empty for a side that is itself a set operation, and for a
/// `*` whose width is not known.
pub(crate) fn side_locations(s: &SelectStmt, planned: &Statement) -> Vec<Option<i32>> {
    if s.op != SetOperation::SetopNone as i32 {
        return Vec::new();
    }
    if let Some(first) = s.values_lists.first() {
        let width = match first.node.as_ref() {
            Some(N::List(l)) => l.items.len(),
            _ => 0,
        };
        return vec![Some(-1); width];
    }
    let is_star = |rt: &pg_query::protobuf::ResTarget| {
        matches!(
            rt.val.as_deref().and_then(|v| v.node.as_ref()),
            Some(N::ColumnRef(c)) if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_))))
        )
    };
    let targets: Vec<&pg_query::protobuf::ResTarget> = s
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.as_ref()),
            _ => None,
        })
        .collect();
    let stars = targets.iter().filter(|rt| is_star(rt)).count();
    // A `*` expands to the columns the other entries leave, each a Var at
    // the `*`'s column reference; with more than one the split is unknown.
    let star_width = match (stars, statement_width(planned)) {
        (0, _) => 0,
        (1, Some(w)) if w + 1 >= targets.len() => w + 1 - targets.len(),
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(targets.len());
    for rt in targets {
        let loc = rt.val.as_deref().and_then(analysed_location);
        if is_star(rt) {
            out.extend(std::iter::repeat_n(loc, star_width));
        } else {
            out.push(loc);
        }
    }
    out
}

/// How many output columns a planned leaf side has, when known.
fn statement_width(planned: &Statement) -> Option<usize> {
    match planned {
        Statement::Select(sel) => Some(sel.columns.len()),
        Statement::SelectConstant(sc) => Some(sc.columns.len()),
        Statement::ValuesConstant(vc) => Some(vc.names.len()),
        _ => None,
    }
}

/// `exprLocation` of the expression a raw select-list entry is analysed
/// into: the leftmost location of the node and its leading operand, as
/// `exprLocation` takes it, except that a cast of an UNTYPED literal
/// (`'a'::text`, `cast(null as text)`, `text 'a'`) is coerced in place, a
/// Const that keeps the literal's own location (PostgreSQL 15.19: `select 1
/// union select cast(null as text)` is at the `null`, `... cast(1 as text)`
/// at the `cast`).
pub(crate) fn analysed_location(node: &Node) -> Option<i32> {
    use pg_query::protobuf::a_const::Val;
    let at = |l: i32| (l >= 0).then_some(l);
    let leftmost = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    };
    let first = |l: &[Node]| l.iter().find_map(analysed_location);
    let of = |n: &Option<Box<Node>>| n.as_deref().and_then(analysed_location);
    match node.node.as_ref()? {
        N::TypeCast(t) => {
            if let Some(N::AConst(c)) = t.arg.as_deref().and_then(|a| a.node.as_ref()) {
                if c.isnull || matches!(c.val, Some(Val::Sval(_))) {
                    return at(c.location);
                }
            }
            leftmost(at(t.location), of(&t.arg))
        }
        N::AConst(c) => at(c.location),
        N::ColumnRef(c) => at(c.location),
        N::ParamRef(p) => at(p.location),
        N::FuncCall(f) => leftmost(at(f.location), first(&f.args)),
        N::AExpr(e) => leftmost(at(e.location), of(&e.lexpr)),
        N::BoolExpr(b) => leftmost(at(b.location), first(&b.args)),
        N::NullTest(t) => leftmost(at(t.location), of(&t.arg)),
        N::BooleanTest(t) => leftmost(at(t.location), of(&t.arg)),
        N::CollateClause(c) => leftmost(of(&c.arg), at(c.location)),
        N::SubLink(s) => leftmost(of(&s.testexpr), at(s.location)),
        N::AIndirection(i) => of(&i.arg),
        N::CaseExpr(c) => at(c.location),
        N::CoalesceExpr(c) => at(c.location),
        N::MinMaxExpr(m) => at(m.location),
        N::RowExpr(r) => at(r.location),
        N::AArrayExpr(a) => at(a.location),
        N::SqlvalueFunction(f) => at(f.location),
        N::GroupingFunc(g) => at(g.location),
        N::XmlExpr(x) => at(x.location),
        _ => None,
    }
}
