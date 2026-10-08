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
//! - only where the substituted values can land:
//!   - the WHERE filter of a SELECT, UPDATE or DELETE;
//!   - an expression the executor evaluates per row (an UPDATE's
//!     `SET v = v + $1`, a residual WHERE), which carries the statement's
//!     values and is checked against its column when it runs;
//!   - a value bound for STORAGE (an INSERT's row, an UPDATE's `SET v = $1`),
//!     but only into a column no value of that kind can fail to fit
//!     (`stores_verbatim`). The planner checks a stored value against its
//!     column -- a varchar length, an integer range, a domain -- and a
//!     stand-in passing that check says nothing about the next value, so a
//!     column with any such check is never templated.
//!
//!   A value anywhere else (a LIMIT, a computed column) makes the two plans
//!   differ after substitution, and the statement is not templated.
//!
//! A template is valid while the catalog version, the session's settings
//! generation and its role are those it was planned under, and never while
//! the session has uncommitted DDL (its lookups see that DDL).

use bson::{Bson, Document};
use secantus_pgcatalog::Column;
use secantus_pgplan::{ColumnExpr, Statement};

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

/// Can every value of `v`'s BSON type be stored in `column` exactly as it
/// is, with nothing to check? True for the four pairings where the column's
/// type holds the whole range of the value's and declares no width, domain,
/// enum or generation: `int4` <- int32, `int8` <- int64, `float8` <- double,
/// `text` <- string.
pub(crate) fn stores_verbatim(v: &Bson, column: &Column) -> bool {
    column.typmod == -1
        && column.extra.is_empty()
        && column.field_override.is_none()
        && matches!(
            (v, column.pg_type.as_str()),
            (Bson::Int32(_), "int4")
                | (Bson::Int64(_), "int8")
                | (Bson::Double(_), "float8")
                | (Bson::String(_), "text")
        )
}

/// The top-level values of `d` only: a stored row's own fields. A stand-in
/// deeper in (an array element, a composite key's part) is left alone, so
/// such a statement never passes the template check.
fn sub_fields(d: &mut Document, from: &[Bson], to: &[Bson], hits: &mut [bool]) {
    for (_, v) in d.iter_mut() {
        if let Some(i) = from.iter().position(|f| same(f, v)) {
            *v = to[i].clone();
            hits[i] = true;
        }
    }
}

/// A per-row expression carries the statement's values whole (`params`),
/// read by `$n` when it is evaluated. Where they are exactly `from` they
/// become `to`, and each `$n` the expression reads counts as found.
fn sub_expr(e: &mut ColumnExpr, from: &[Bson], to: &[Bson], hits: &mut [bool]) {
    let ColumnExpr::Row { expr, params, .. } = e else {
        return;
    };
    if params.len() != from.len() || !params.iter().zip(from).all(|(p, f)| same(p, f)) {
        return;
    }
    params.clone_from_slice(to);
    let Some(root) = expr.node.as_ref() else {
        return;
    };
    for (node, ..) in root.nodes() {
        if let pg_query::NodeRef::ParamRef(p) = node {
            if let Some(hit) = usize::try_from(p.number)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| hits.get_mut(i))
            {
                *hit = true;
            }
        }
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
        Statement::Insert(i) => {
            if i.source.is_some() {
                return None;
            }
            for row in &mut i.rows {
                sub_fields(row, from, to, h);
            }
        }
        Statement::Update(u) => {
            sub_fields(&mut u.set, from, to, h);
            for (_, _, _, e) in &mut u.set_exprs {
                sub_expr(e, from, to, h);
            }
            sub_doc(&mut u.filter, from, to, h);
            if let Some(e) = &mut u.residual {
                sub_expr(e, from, to, h);
            }
        }
        Statement::Delete(d) => {
            sub_doc(&mut d.filter, from, to, h);
            if let Some(e) = &mut d.residual {
                sub_expr(e, from, to, h);
            }
        }
        _ => return None,
    }
    Some((s, hits))
}

/// Where `plan` STORES one of the stand-ins `a`: `(table, field, value)` for
/// each field of an INSERT's rows or an UPDATE's SET holding one. The caller
/// checks each against its column (`stores_verbatim`).
pub(crate) fn stored_stand_ins<'p>(
    plan: &'p Statement,
    a: &[Bson],
) -> Vec<(&'p str, &'p str, &'p Bson)> {
    let (table, docs): (&str, Vec<&Document>) = match plan {
        Statement::Insert(i) => (&i.table, i.rows.iter().collect()),
        Statement::Update(u) => (&u.table, vec![&u.set]),
        _ => return Vec::new(),
    };
    docs.into_iter()
        .flat_map(|d| d.iter())
        .filter(|(_, v)| a.iter().any(|s| same(s, v)))
        .map(|(k, v)| (table, k.as_str(), v))
        .collect()
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

    #[test]
    fn stores_verbatim_only_where_no_value_can_fail() {
        let col = |t: &str| Column::new("c", t, false);
        let text = || Bson::String("x".into());
        assert!(stores_verbatim(&Bson::Int32(1), &col("int4")));
        assert!(stores_verbatim(&Bson::Int64(1), &col("int8")));
        assert!(stores_verbatim(&Bson::Double(1.0), &col("float8")));
        assert!(stores_verbatim(&text(), &col("text")));
        // A narrower range, a conversion, a parse.
        assert!(!stores_verbatim(&Bson::Int32(1), &col("int2")));
        assert!(!stores_verbatim(&Bson::Int32(1), &col("int8")));
        assert!(!stores_verbatim(&Bson::Int64(1), &col("int4")));
        assert!(!stores_verbatim(&Bson::Double(1.0), &col("float4")));
        assert!(!stores_verbatim(&text(), &col("varchar")));
        assert!(!stores_verbatim(&text(), &col("date")));
        assert!(!stores_verbatim(&Bson::Boolean(true), &col("bool")));
        // A declared width, and a domain / enum / generated column.
        let mut wide = col("text");
        wide.typmod = 12;
        assert!(!stores_verbatim(&text(), &wide));
        let mut domain = col("text");
        domain.extra.insert("domain_type", "d");
        assert!(!stores_verbatim(&text(), &domain));
    }

    #[test]
    fn stored_stand_ins_are_top_level_fields_only() {
        let a = [Bson::Int32(23_011)];
        let mut row = Document::new();
        row.insert("_id", Bson::Int32(23_011));
        row.insert("arr", Bson::Array(vec![Bson::Int32(23_011)]));
        let mut hits = [false];
        sub_fields(&mut row, &a, &[Bson::Int32(5)], &mut hits);
        assert_eq!(row.get("_id"), Some(&Bson::Int32(5)));
        assert_eq!(
            row.get("arr"),
            Some(&Bson::Array(vec![Bson::Int32(23_011)]))
        );
        assert!(hits[0]);
    }
}
