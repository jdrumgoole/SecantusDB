//! `SS_process_sublinks` for an EXPLAINed SELECT (RP-440): an
//! uncorrelated scalar subquery compared with a column of the one relation
//! the SELECT reads is an InitPlan, run once before the scan, and the qual
//! reads its output parameter: `b = (select 'v3'::text)` is `Filter: (b =
//! $0)` over `InitPlan 1 (returns $0)`.
//!
//! Recognised here: a SELECT over one plain relation whose WHERE's AND
//! conjuncts are each a comparison of a column with such a subquery (a
//! FROM-less one-target SELECT), or of a column with a constant. Anything
//! else is left to the plan as written.

use super::*;
use pg_query::protobuf::{AExprKind, ExplainStmt, SubLink, SubLinkType};

/// One InitPlan the scan's quals read, as EXPLAIN prints and prices it.
#[derive(Debug, Clone, PartialEq)]
pub struct InitPlan {
    /// The scan's stored field the comparison reads.
    pub field: String,
    /// The comparison, the column on its left.
    pub op: colstats::Op,
    /// The subquery, planned as EXPLAIN plans it on its own.
    pub plan: Box<Statement>,
}

fn conjuncts<'a>(n: &'a pg_query::protobuf::Node, out: &mut Vec<&'a pg_query::protobuf::Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                conjuncts(a, out);
            }
        }
        _ => out.push(n),
    }
}

fn op_of(name: &[pg_query::protobuf::Node]) -> Option<colstats::Op> {
    let [n] = name else { return None };
    let Some(N::String(s)) = n.node.as_ref() else {
        return None;
    };
    Some(match s.sval.as_str() {
        "=" => colstats::Op::Eq,
        "<>" | "!=" => colstats::Op::Ne,
        "<" => colstats::Op::Lt,
        "<=" => colstats::Op::Le,
        ">" => colstats::Op::Gt,
        ">=" => colstats::Op::Ge,
        _ => return None,
    })
}

fn commuted(op: colstats::Op) -> colstats::Op {
    use colstats::Op::*;
    match op {
        Lt => Gt,
        Le => Ge,
        Gt => Lt,
        Ge => Le,
        o => o,
    }
}

fn column<'a>(n: &pg_query::protobuf::Node, def: &'a TableDef, rel: &str) -> Option<&'a Column> {
    let Some(N::ColumnRef(c)) = n.node.as_ref() else {
        return None;
    };
    let parts: Vec<&str> = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let col = match parts.as_slice() {
        [col] => *col,
        [r, col] if *r == rel => *col,
        _ => return None,
    };
    def.columns.iter().find(|c| c.name == col)
}

/// A FROM-less one-target SELECT with nothing else: what an InitPlan
/// over a `Result` plans.
fn fromless_scalar(sl: &SubLink) -> bool {
    if SubLinkType::try_from(sl.sub_link_type) != Ok(SubLinkType::ExprSublink) {
        return false;
    }
    match sl.subselect.as_deref().and_then(|n| n.node.as_ref()) {
        Some(N::SelectStmt(s)) => {
            s.from_clause.is_empty()
                && s.where_clause.is_none()
                && s.target_list.len() == 1
                && s.op == pg_query::protobuf::SetOperation::SetopNone as i32
                && s.with_clause.is_none()
                && s.group_clause.is_empty()
                && s.having_clause.is_none()
                && s.values_lists.is_empty()
                && s.limit_count.is_none()
                && s.limit_offset.is_none()
                && s.sort_clause.is_empty()
        }
        _ => false,
    }
}

fn sub(n: &pg_query::protobuf::Node) -> Option<&SubLink> {
    match n.node.as_ref() {
        Some(N::SubLink(sl)) if fromless_scalar(sl) => Some(sl.as_ref()),
        _ => None,
    }
}

fn constant(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::AConst(_)) => true,
        Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(constant),
        _ => false,
    }
}

/// The InitPlans `query`'s WHERE makes, in their order, with the sublinks
/// they replace (for the qual printer); none when it is not that shape.
pub(crate) fn init_plans_of(
    query: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<(Vec<SubLink>, Vec<InitPlan>)> {
    let none = Ok((Vec::new(), Vec::new()));
    let N::SelectStmt(s) = query else {
        return none;
    };
    let [item] = s.from_clause.as_slice() else {
        return none;
    };
    let Some(N::RangeVar(rv)) = item.node.as_ref() else {
        return none;
    };
    let (Some(w), Some(def)) = (s.where_clause.as_deref(), lookup(&rv.relname)) else {
        return none;
    };
    let rel = rv
        .alias
        .as_ref()
        .map_or(rv.relname.as_str(), |a| a.aliasname.as_str());
    let mut parts = Vec::new();
    conjuncts(w, &mut parts);
    let mut found: Vec<(&SubLink, String, colstats::Op)> = Vec::new();
    for p in parts {
        let Some(N::AExpr(e)) = p.node.as_ref() else {
            return none;
        };
        if e.kind != AExprKind::AexprOp as i32 {
            return none;
        }
        let (Some(op), Some(l), Some(r)) = (op_of(&e.name), e.lexpr.as_deref(), e.rexpr.as_deref())
        else {
            return none;
        };
        if let (Some(c), Some(sl)) = (column(l, &def, rel), sub(r)) {
            found.push((sl, c.field(), op));
        } else if let (Some(sl), Some(c)) = (sub(l), column(r, &def, rel)) {
            found.push((sl, c.field(), commuted(op)));
        } else if !((column(l, &def, rel).is_some() && constant(r))
            || (constant(l) && column(r, &def, rel).is_some()))
        {
            return none;
        }
    }
    let mut sublinks = Vec::new();
    let mut plans = Vec::new();
    for (sl, field, op) in found {
        let e = ExplainStmt {
            query: sl.subselect.clone(),
            options: Vec::new(),
        };
        let plan = match plan_explain(&e, lookup, params)? {
            Statement::Explain { inner, .. } => inner,
            _ => return none,
        };
        sublinks.push(sl.clone());
        plans.push(InitPlan { field, op, plan });
    }
    Ok((sublinks, plans))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn where_of(sql: &str) -> N {
        let parsed = pg_query::parse(sql).unwrap();
        parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap()
    }

    fn e(name: &str) -> Option<TableDef> {
        (name == "e").then(|| {
            TableDef::new(
                "e",
                vec![
                    Column::new("a", "int4", false),
                    Column::new("b", "text", false),
                ],
            )
        })
    }

    /// Each scalar sublink a column is compared with is an InitPlan, in
    /// order, a commuted one read with the column on the left, and the
    /// quals print its parameter (RP-440).
    #[test]
    fn sublinks_are_numbered_initplans() {
        let q = where_of("select * from e where a > (select 5) and (select 'v3'::text) < b");
        let (sublinks, plans) = init_plans_of(&q, &e, &[]).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(
            (plans[0].field.as_str(), plans[0].op),
            ("a", colstats::Op::Gt)
        );
        assert_eq!(
            (plans[1].field.as_str(), plans[1].op),
            ("b", colstats::Op::Gt)
        );
        let N::SelectStmt(s) = &q else { unreachable!() };
        let quals = ruleutils::with_init_plans(&sublinks, || {
            ruleutils::explain_conjuncts(s.where_clause.as_deref().unwrap(), &e("e").unwrap())
        })
        .unwrap();
        let texts: Vec<&str> = quals.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["(a > $0)", "($1 < b)"]);
    }

    /// A sublink beside a qual that is neither is left as written.
    #[test]
    fn another_qual_declines() {
        let q = where_of("select * from e where b = (select 'v3'::text) and a + 1 > 2");
        assert!(init_plans_of(&q, &e, &[]).unwrap().1.is_empty());
    }
}
