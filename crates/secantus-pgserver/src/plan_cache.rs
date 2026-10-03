//! Plan reuse for a statement executed again with new parameter values (a
//! prepared statement's Execute, the hot path of every driver).
//!
//! The planner inlines bound values into the plan, so a statement planned
//! once cannot simply be run again with other values. Instead a TEMPLATE is
//! learned: the statement is planned with two distinct sets of stand-in
//! values (`sentinels`), and if substituting the second set for the first in
//! the first plan gives exactly the second plan, every parameter was copied
//! into the plan verbatim -- the planner made no decision on its value -- and
//! later executions substitute their own values into the template instead of
//! planning again.
//!
//! The check is what makes it safe, so the template is only TRIED where a
//! value could not have been read in a way the check misses:
//! - one SELECT / INSERT / UPDATE / DELETE with no function call, no
//!   subquery or CTE and no SQL value function (`now()`, `current_user`,
//!   `nextval` -- planned once, they would freeze), and no planning-time
//!   subquery run, over plain tables only: no view, no table with a rule,
//!   and no row-level security anywhere in the database (each can bring a
//!   function the statement's text does not show);
//! - parameters of a few BSON types (int32 / int64 / double / string) whose
//!   stand-ins are distinct in every way the planner might branch on: both
//!   in range of the smallest integer type, strings carrying leading and
//!   trailing blanks and mixed case (so a trim, pad or case fold shows);
//! - an execution whose values have the stand-ins' BSON types and no NULL
//!   (NULL changes a plan's meaning: `= NULL` is never true);
//! - only where the substituted values can land: the WHERE filter of a
//!   SELECT, UPDATE or DELETE. A value anywhere else (a LIMIT, a computed
//!   column, an UPDATE's SET, an INSERT's row) makes the two plans differ
//!   after substitution, and the statement is not templated. A value bound
//!   for storage is excluded on purpose: the planner checks it against its
//!   column (a varchar length, an integer range, a domain), and a stand-in
//!   passing that check says nothing about the next value.
//!
//! A template is valid while the catalog version, the session's settings
//! generation and its role are those it was planned under, and never while
//! the session has uncommitted DDL (its lookups see that DDL).

use bson::{Bson, Document};
use secantus_pgplan::Statement;

/// May `sql` be templated at all, and over which relations? (A
/// statement-shape check, made once per statement text; the caller then
/// checks each relation is a plain stored table -- a view, a rule or a
/// row-security policy can bring a function the text does not show.)
pub(crate) fn eligible_sql(sql: &str) -> Option<Vec<String>> {
    let parsed = pg_query::parse(sql).ok()?;
    let stmts = &parsed.protobuf.stmts;
    if stmts.len() != 1 {
        return None;
    }
    let shape_ok = matches!(
        stmts[0].stmt.as_ref().and_then(|n| n.node.as_ref()),
        Some(
            pg_query::NodeEnum::SelectStmt(_)
                | pg_query::NodeEnum::InsertStmt(_)
                | pg_query::NodeEnum::UpdateStmt(_)
                | pg_query::NodeEnum::DeleteStmt(_)
        )
    );
    let plain = shape_ok
        && !parsed.protobuf.nodes().iter().any(|(n, ..)| {
            matches!(
                n,
                pg_query::NodeRef::FuncCall(_)
                    | pg_query::NodeRef::SubLink(_)
                    | pg_query::NodeRef::RangeSubselect(_)
                    | pg_query::NodeRef::RangeFunction(_)
                    | pg_query::NodeRef::CommonTableExpr(_)
                    | pg_query::NodeRef::WithClause(_)
                    | pg_query::NodeRef::SqlvalueFunction(_)
                    | pg_query::NodeRef::LockingClause(_)
                    | pg_query::NodeRef::OnConflictClause(_)
            )
        });
    plain.then(|| parsed.tables())
}

/// The two stand-in sets for `params`, or `None` when one of them cannot be
/// stood in for (a NULL, or a type outside the handled four).
pub(crate) fn sentinels(params: &[Bson]) -> Option<(Vec<Bson>, Vec<Bson>)> {
    let mut a = Vec::with_capacity(params.len());
    let mut b = Vec::with_capacity(params.len());
    for (i, p) in params.iter().enumerate() {
        let i32_ = i as i32;
        let (x, y) = match p {
            // Inside int2's range, so a smallint column's range check passes
            // for both, and distinct from each other and from every index.
            Bson::Int32(_) => (
                Bson::Int32(23_011 + 2 * i32_),
                Bson::Int32(23_012 + 2 * i32_),
            ),
            Bson::Int64(_) => (
                Bson::Int64(23_011 + 2 * i64::from(i32_)),
                Bson::Int64(23_012 + 2 * i64::from(i32_)),
            ),
            Bson::Double(_) => (
                Bson::Double(23_011.25 + f64::from(i32_)),
                Bson::Double(23_511.75 + f64::from(i32_)),
            ),
            Bson::String(_) => (
                Bson::String(format!(" Sx{i}a ")),
                Bson::String(format!(" Sx{i}b ")),
            ),
            _ => return None,
        };
        a.push(x);
        b.push(y);
    }
    Some((a, b))
}

/// Do `params` fit a template learned with stand-ins `like`: same BSON type
/// each, none NULL?
pub(crate) fn fits(params: &[Bson], like: &[Bson]) -> bool {
    params.len() == like.len()
        && params
            .iter()
            .zip(like)
            .all(|(p, l)| std::mem::discriminant(p) == std::mem::discriminant(l))
}

fn sub_bson(v: &mut Bson, from: &[Bson], to: &[Bson], hits: &mut [bool]) {
    match v {
        Bson::Document(d) => sub_doc(d, from, to, hits),
        Bson::Array(items) => {
            for item in items {
                sub_bson(item, from, to, hits);
            }
        }
        other => {
            if let Some(i) = from.iter().position(|f| same(f, other)) {
                *other = to[i].clone();
                hits[i] = true;
            }
        }
    }
}

/// Exact identity: type and value (`Bson`'s `==` is that, except that a
/// double NaN is never equal; the stand-ins are never NaN).
fn same(a: &Bson, b: &Bson) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b) && a == b
}

fn sub_doc(d: &mut Document, from: &[Bson], to: &[Bson], hits: &mut [bool]) {
    for (_, v) in d.iter_mut() {
        sub_bson(v, from, to, hits);
    }
}

/// `stmt` with every stand-in in `from` replaced by its value in `to`, in the
/// places a parameter may land; `None` for a statement kind not templated.
pub(crate) fn substitute(stmt: &Statement, from: &[Bson], to: &[Bson]) -> Option<Statement> {
    substitute_counting(stmt, from, to).map(|(s, _)| s)
}

/// `substitute`, and which of `from` were found.
fn substitute_counting(
    stmt: &Statement,
    from: &[Bson],
    to: &[Bson],
) -> Option<(Statement, Vec<bool>)> {
    let mut hits = vec![false; from.len()];
    let h = &mut hits;
    let mut s = stmt.clone();
    match &mut s {
        Statement::Select(sel) => {
            if sel.sub.is_some() || sel.join.is_some() {
                return None;
            }
            sub_doc(&mut sel.filter, from, to, h);
        }
        // The filter only: a value bound for STORAGE (an UPDATE's SET, an
        // INSERT's row) is checked against its column while planning -- a
        // length, a range, a domain -- and a stand-in that passes says
        // nothing about the value that comes later. A comparison checks
        // nothing of the kind.
        Statement::Update(u) => sub_doc(&mut u.filter, from, to, h),
        Statement::Delete(d) => sub_doc(&mut d.filter, from, to, h),
        _ => return None,
    }
    Some((s, hits))
}

/// Is `plan_a` (planned with stand-ins `a`) a template: does substituting
/// `b` for `a` in it give exactly `plan_b` (planned with `b`)? And does every
/// stand-in actually appear in it, so a value is never silently dropped?
pub(crate) fn is_template(plan_a: &Statement, plan_b: &Statement, a: &[Bson], b: &[Bson]) -> bool {
    if a.is_empty() {
        return plan_a == plan_b;
    }
    let Some((moved, hits)) = substitute_counting(plan_a, a, b) else {
        return false;
    };
    // Every stand-in must have been found where substitution looks: one
    // folded away, or landed anywhere else, is a value this cannot carry.
    moved == *plan_b && hits.into_iter().all(|h| h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility() {
        assert_eq!(
            eligible_sql("select v from t where k = $1"),
            Some(vec!["t".to_string()])
        );
        assert!(eligible_sql("update t set v = 'x' where k = $1").is_some());
        assert_eq!(eligible_sql("select 1"), Some(Vec::new()));
        assert!(eligible_sql("select now() from t where k = $1").is_none());
        assert!(eligible_sql("select v from t where k = (select 1)").is_none());
        assert!(eligible_sql("select 1; select 2").is_none());
        assert!(eligible_sql("select current_user").is_none());
        assert!(eligible_sql("create table x (a int)").is_none());
    }

    #[test]
    fn sentinels_refuse_null_and_bool() {
        assert!(sentinels(&[Bson::Null]).is_none());
        assert!(sentinels(&[Bson::Boolean(true)]).is_none());
        let (a, b) = sentinels(&[Bson::Int32(5), Bson::String("x".into())]).expect("ok");
        assert_ne!(a, b);
        assert!(fits(&[Bson::Int32(9), Bson::String("y".into())], &a));
        assert!(!fits(&[Bson::Int64(9), Bson::String("y".into())], &a));
    }
}
