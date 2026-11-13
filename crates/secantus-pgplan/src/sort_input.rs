//! Which of a select list's columns a query with ORDER BY computes BELOW
//! its Sort, and which it postpones to a projection above it: PostgreSQL
//! 15.19's `make_sort_input_target` (planner.c), with `cost_qual_eval`
//! (costsize.c) deciding which columns are "expensive" (RP-732).
//!
//! PostgreSQL's Sort reads rows that already carry the select list, so
//! each one is computed once per row, in scan order, before any row is
//! sorted: `select k, ('x' || k)::int from v order by k desc` raises the
//! FIRST scanned row's cast error, whatever sorts first. A column is left
//! for after the Sort only when it is no sort key and the select list has
//! a volatile column (always postponed, so `random()` runs in output order
//! and only for the rows a LIMIT keeps), or an expensive one with a LIMIT
//! (postponed so the Sort's discarded rows never pay for it). When neither
//! holds, every column is computed below the Sort.

use super::*;

/// "Expensive": more than `10 * cpu_operator_cost` a row.
const EXPENSIVE_OPERATORS: f64 = 10.0;

/// One flag per output column of `columns`, the select list `s` planned
/// to: `true` for a column computed below the Sort, `false` for one
/// postponed until after it. Empty when `s` has no ORDER BY (there is no
/// Sort), and when the columns cannot be matched to `s`'s targets one for
/// one -- a select list with more than one star -- which leaves every
/// computed column to the projection after the Sort, as before RP-732.
///
/// `tuple_fraction > 0` (a cursor's `cursor_tuple_fraction`) is not seen
/// here: a DECLARE CURSOR is planned as a plain query, so only a LIMIT
/// postpones an expensive column.
pub(crate) fn sort_input_columns(
    s: &pg_query::protobuf::SelectStmt,
    columns: &[(String, String)],
) -> Vec<bool> {
    if s.sort_clause.is_empty() {
        return Vec::new();
    }
    let targets: Vec<&pg_query::protobuf::ResTarget> = s
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.as_ref()),
            _ => None,
        })
        .collect();
    let Some(spans) = target_spans(&targets, columns.len()) else {
        return Vec::new();
    };
    // Per output column, the target it came from (`None` for a star's).
    let mut origin: Vec<Option<&pg_query::protobuf::ResTarget>> = Vec::new();
    for (rt, span) in targets.iter().zip(&spans) {
        if is_star(rt) {
            origin.extend(std::iter::repeat_n(None, *span));
        } else {
            origin.push(Some(*rt));
        }
    }
    // `get_pathtarget_sortgroupref(...) == 0`: what ORDER BY names is a
    // sort key, computed below the Sort whatever it costs.
    let postponable: Vec<Option<Postpone>> = origin
        .iter()
        .enumerate()
        .map(|(i, rt)| {
            let rt = (*rt)?;
            let val = rt.val.as_deref()?;
            if sorts_by(s, i, &columns[i].0, val) {
                return None;
            }
            if correlated::has_volatile_call(val) {
                Some(Postpone::Volatile)
            } else if per_tuple_operators(val) > EXPENSIVE_OPERATORS {
                Some(Postpone::Expensive)
            } else {
                None
            }
        })
        .collect();
    let have_volatile = postponable.contains(&Some(Postpone::Volatile));
    let have_expensive = postponable.contains(&Some(Postpone::Expensive));
    // `parse->limitCount`: written at all (`LIMIT ALL` included).
    let post_sort = have_volatile || (have_expensive && s.limit_count.is_some());
    postponable
        .iter()
        .map(|p| !post_sort || p.is_none())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Postpone {
    Volatile,
    Expensive,
}

/// How many output columns each target expands to: one, except a star
/// (`*`, `t.*`, `(v).*`), which takes what the others leave. `None` when
/// that does not settle it (two stars, or a count that does not add up).
fn target_spans(targets: &[&pg_query::protobuf::ResTarget], columns: usize) -> Option<Vec<usize>> {
    let stars = targets.iter().filter(|rt| is_star(rt)).count();
    let plain = targets.len() - stars;
    match stars {
        0 if plain == columns => Some(vec![1; plain]),
        1 if columns >= plain => Some(
            targets
                .iter()
                .map(|rt| if is_star(rt) { columns - plain } else { 1 })
                .collect(),
        ),
        _ => None,
    }
}

fn is_star(rt: &pg_query::protobuf::ResTarget) -> bool {
    let star = |n: &pg_query::protobuf::Node| matches!(n.node.as_ref(), Some(N::AStar(_)));
    match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
        Some(N::ColumnRef(c)) => c.fields.last().is_some_and(star),
        Some(N::AIndirection(a)) => a.indirection.last().is_some_and(star),
        _ => false,
    }
}

/// Does an ORDER BY item of `s` name output column `i` (named `out`,
/// computing `val`)? By position, by output name (`findTargetlistEntrySQL92`)
/// or as the same expression (`findTargetlistEntrySQL99`).
fn sorts_by(
    s: &pg_query::protobuf::SelectStmt,
    i: usize,
    out: &str,
    val: &pg_query::protobuf::Node,
) -> bool {
    s.sort_clause.iter().any(|item| {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return false;
        };
        let Some(key) = sb.node.as_deref() else {
            return false;
        };
        match key.node.as_ref() {
            Some(N::AConst(c)) => match c.val.as_ref() {
                Some(pg_query::protobuf::a_const::Val::Ival(v)) => {
                    usize::try_from(v.ival).is_ok_and(|p| p == i + 1)
                }
                _ => false,
            },
            Some(N::ColumnRef(c))
                if c.fields.len() == 1 && column_ref_name(c).as_deref() == Some(out) =>
            {
                true
            }
            _ => same_expression(key, val),
        }
    })
}

/// `cost_qual_eval`'s per-row cost of `n`, in `cpu_operator_cost` units:
/// each operator and function its `procost`, a constant subtree nothing
/// (`eval_const_expressions` has folded it), a column, a parameter, AND /
/// OR / NOT, an aggregate or a window function nothing, and a correlated
/// subquery (a SubPlan) its plan's cost a call.
///
/// Read off the parse tree, which has no resolved types, so three charges
/// are approximated: a cast is one function call (PostgreSQL: none for a
/// binary-coercible one, two for an I/O conversion); an operator or a
/// function whose overloads cost differently is charged its cheapest; and
/// a correlated subquery is charged as expensive outright, as a SubPlan
/// over any table is.
fn per_tuple_operators(n: &pg_query::protobuf::Node) -> f64 {
    use pg_query::protobuf::AExprKind as K;
    if folds_to_constant(n) {
        return 0.0;
    }
    let one =
        |n: &Option<Box<pg_query::protobuf::Node>>| n.as_deref().map_or(0.0, per_tuple_operators);
    let all = |v: &[pg_query::protobuf::Node]| v.iter().map(per_tuple_operators).sum::<f64>();
    let Some(inner) = n.node.as_ref() else {
        return 0.0;
    };
    match inner {
        N::AConst(_) | N::ColumnRef(_) | N::ParamRef(_) => 0.0,
        N::TypeCast(tc) => 1.0 + one(&tc.arg),
        N::CollateClause(c) => one(&c.arg),
        N::AExpr(e) => {
            let operands = one(&e.lexpr) + one(&e.rexpr);
            let op = || operator_name(e).map_or(1.0, operator_procost);
            match K::try_from(e.kind) {
                Ok(K::AexprOp) => op() + operands,
                // A ScalarArrayOpExpr: the operator over half the array.
                Ok(K::AexprOpAny | K::AexprOpAll) => {
                    op() * array_length(e.rexpr.as_deref()) * 0.5 + operands
                }
                Ok(K::AexprIn) => {
                    let items = match e.rexpr.as_deref().and_then(|r| r.node.as_ref()) {
                        Some(N::List(l)) => l.items.len() as f64,
                        _ => 1.0,
                    };
                    // A list of nine or more constants is hashed: one hash
                    // and one comparison a row (`MIN_ARRAY_SIZE_FOR_HASHED_SAOP`).
                    if items >= 9.0 && e.rexpr.as_deref().is_some_and(folds_to_constant) {
                        2.0 + operands
                    } else {
                        op() * items * 0.5 + operands
                    }
                }
                Ok(K::AexprBetween | K::AexprNotBetween) => 2.0 + operands,
                Ok(K::AexprBetweenSym | K::AexprNotBetweenSym) => 4.0 + operands,
                // `similar_to_escape` and the `~` it feeds.
                Ok(K::AexprSimilar) => 2.0 + operands,
                // DistinctExpr, NullIfExpr, LIKE's `~~`: one operator.
                _ => 1.0 + operands,
            }
        }
        N::BoolExpr(b) => all(&b.args),
        N::NullTest(t) => one(&t.arg),
        N::BooleanTest(t) => one(&t.arg),
        N::CaseExpr(c) => {
            // A simple CASE compares its argument with each WHEN: an `=` each.
            let simple = if c.arg.is_some() {
                c.args.len() as f64
            } else {
                0.0
            };
            let whens: f64 = c
                .args
                .iter()
                .map(|w| match w.node.as_ref() {
                    Some(N::CaseWhen(cw)) => one(&cw.expr) + one(&cw.result),
                    _ => 0.0,
                })
                .sum();
            simple + one(&c.arg) + whens + one(&c.defresult)
        }
        N::CoalesceExpr(c) => all(&c.args),
        N::MinMaxExpr(m) => 1.0 + all(&m.args),
        N::SqlvalueFunction(_) => 1.0,
        N::XmlExpr(x) => 1.0 + all(&x.named_args) + all(&x.args),
        N::RowExpr(r) => all(&r.args),
        N::AArrayExpr(a) => all(&a.elements),
        N::AIndirection(a) => {
            one(&a.arg)
                + a.indirection
                    .iter()
                    .map(|ind| match ind.node.as_ref() {
                        Some(N::AIndices(idx)) => one(&idx.lidx) + one(&idx.uidx),
                        _ => 0.0,
                    })
                    .sum::<f64>()
        }
        N::NamedArgExpr(a) => one(&a.arg),
        // An uncorrelated subquery is an InitPlan, run once: its Param
        // costs a row nothing. Its test expression is still evaluated.
        N::SubLink(sl) => one(&sl.testexpr),
        N::FuncCall(f) => {
            if f.over.is_some() || f.agg_star || is_aggregate_call(f) {
                return 0.0;
            }
            if correlated::is_correlated(f) {
                return EXPENSIVE_OPERATORS + 1.0;
            }
            let args = all(&f.args);
            let procost = match func_name(f) {
                Some(name) => match correlated::user_function_for(&name, &f.args) {
                    Some(u) => u.cost,
                    None => builtin_procost(&name),
                },
                None => 1.0,
            };
            procost + args
        }
        _ => 0.0,
    }
}

/// `estimate_array_length`: a constant array's elements, else 10.
fn array_length(n: Option<&pg_query::protobuf::Node>) -> f64 {
    match n.and_then(|n| n.node.as_ref()) {
        Some(N::AArrayExpr(a)) => a.elements.len() as f64,
        Some(N::TypeCast(tc)) => match tc.arg.as_deref().and_then(|a| a.node.as_ref()) {
            Some(N::AArrayExpr(a)) => a.elements.len() as f64,
            Some(N::AConst(pg_query::protobuf::AConst {
                val: Some(pg_query::protobuf::a_const::Val::Sval(s)),
                ..
            })) => array_literal_length(&s.sval),
            _ => 10.0,
        },
        _ => 10.0,
    }
}

/// The top-level elements of a one-dimensional array literal (`'{1,2,3}'`).
fn array_literal_length(text: &str) -> f64 {
    let body = text.trim();
    let Some(body) = body.strip_prefix('{').and_then(|b| b.strip_suffix('}')) else {
        return 10.0;
    };
    if body.trim().is_empty() {
        return 0.0;
    }
    if body.contains(['{', '"']) {
        return 10.0;
    }
    body.split(',').count() as f64
}

/// A built-in function's `procost` (PostgreSQL 15.19's `pg_proc`, read off
/// `pg15_function_defs.tsv`: its `COST`, else the language's default --
/// 1 for `internal` / `c`, 100 for `sql`), the cheapest of its overloads.
/// A name it does not know costs 1.
pub(crate) fn builtin_procost(name: &str) -> f64 {
    procosts().get(name).copied().unwrap_or(1.0)
}

/// A built-in function's `procost` where every overload of `name` has
/// the same one, so the overload a call resolves to cannot change it;
/// `None` where they differ or the name is not a built-in.
pub(crate) fn builtin_uniform_procost(name: &str) -> Option<f64> {
    static MAX: std::sync::OnceLock<std::collections::HashMap<String, f64>> =
        std::sync::OnceLock::new();
    let max = MAX.get_or_init(|| {
        let mut out: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for (name, cost) in function_costs() {
            out.entry(name)
                .and_modify(|c| *c = c.max(cost))
                .or_insert(cost);
        }
        out
    });
    let low = procosts().get(name)?;
    (max.get(name) == Some(low)).then_some(*low)
}

/// Every built-in function's `(name, procost)`, an overload a row.
fn function_costs() -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for line in include_str!("pg15_function_defs.tsv").lines() {
        let Some((_, def)) = line.split_once('\t') else {
            continue;
        };
        let Some(rest) = def.split_once("FUNCTION pg_catalog.").map(|(_, r)| r) else {
            continue;
        };
        let Some(name) = rest.split_once('(').map(|(n, _)| n.trim_matches('"')) else {
            continue;
        };
        let language = def
            .split_once(" LANGUAGE ")
            .and_then(|(_, r)| r.split(['\\', ' ']).next())
            .unwrap_or("internal");
        let cost = def
            .split_once(" COST ")
            .and_then(|(_, r)| r.split(['\\', ' ']).next())
            .and_then(|c| c.parse::<f64>().ok())
            .unwrap_or(if matches!(language, "sql" | "plpgsql") {
                100.0
            } else {
                1.0
            });
        out.push((name.to_string(), cost));
    }
    out
}

fn procosts() -> &'static std::collections::HashMap<String, f64> {
    static COSTS: std::sync::OnceLock<std::collections::HashMap<String, f64>> =
        std::sync::OnceLock::new();
    COSTS.get_or_init(|| {
        let mut out: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for (name, cost) in function_costs() {
            out.entry(name)
                .and_modify(|c| *c = c.min(cost))
                .or_insert(cost);
        }
        out
    })
}

/// The `procost` of the function behind a built-in operator
/// (`pg_operator_functions.tsv`), the cheapest of its overloads.
fn operator_procost(op: &str) -> f64 {
    static COSTS: std::sync::OnceLock<std::collections::HashMap<String, f64>> =
        std::sync::OnceLock::new();
    let costs = COSTS.get_or_init(|| {
        let mut out: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for line in include_str!("pg_operator_functions.tsv").lines() {
            let f: Vec<&str> = line.split('\t').collect();
            let (Some(func), Some(op)) = (f.first(), f.last()) else {
                continue;
            };
            let cost = builtin_procost(func);
            out.entry(op.to_string())
                .and_modify(|c| *c = c.min(cost))
                .or_insert(cost);
        }
        out
    });
    let op = if op == "!=" { "<>" } else { op };
    costs.get(op).copied().unwrap_or(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(sql: &str) -> pg_query::protobuf::SelectStmt {
        let parsed = pg_query::parse(sql).expect("parses");
        let stmt = parsed.protobuf.stmts[0].stmt.as_ref().expect("a statement");
        match stmt.node.as_ref() {
            Some(N::SelectStmt(s)) => (**s).clone(),
            _ => panic!("not a select"),
        }
    }

    fn cols(n: usize) -> Vec<(String, String)> {
        (0..n).map(|i| (format!("c{i}"), format!("c{i}"))).collect()
    }

    #[test]
    fn no_order_by_is_no_sort_input() {
        assert!(sort_input_columns(&select("select k, k + 1 from t"), &cols(2)).is_empty());
    }

    #[test]
    fn a_cheap_select_list_is_computed_below_the_sort() {
        let s = select("select k, ('x' || k)::int from t order by k desc");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, true]);
        let s = select("select k, 1 / (k - 1) from t order by k desc limit 1");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, true]);
    }

    #[test]
    fn a_volatile_column_is_postponed_unless_it_is_a_sort_key() {
        let s = select("select k, random() from t order by k");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, false]);
        let s = select("select k, random() as r from t order by r");
        assert_eq!(
            sort_input_columns(&s, &[("k".into(), "k".into()), ("r".into(), "r".into())]),
            vec![true, true]
        );
        let s = select("select k, random() from t order by 2");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, true]);
        let s = select("select k, random() from t order by random()");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, true]);
    }

    #[test]
    fn an_expensive_column_is_postponed_only_under_a_limit() {
        let s = select("select k, to_tsvector('x' || k) from t order by k");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, true]);
        let s = select("select k, to_tsvector('x' || k) from t order by k limit 1");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![true, false]);
        // With a volatile column, an expensive one is postponed too.
        let s = select("select random(), to_tsvector('x' || k) from t order by k");
        assert_eq!(sort_input_columns(&s, &cols(2)), vec![false, false]);
    }

    #[test]
    fn a_star_takes_the_columns_the_other_targets_leave() {
        let s = select("select *, random() from t order by 1");
        assert_eq!(sort_input_columns(&s, &cols(3)), vec![true, true, false]);
        let s = select("select *, t.* from t order by 1");
        assert!(sort_input_columns(&s, &cols(4)).is_empty());
    }

    #[test]
    fn builtin_costs_come_from_pg_proc() {
        assert_eq!(builtin_procost("upper"), 1.0);
        assert_eq!(builtin_procost("to_tsvector"), 100.0);
        assert_eq!(builtin_procost("pg_table_is_visible"), 10.0);
        assert_eq!(operator_procost("+"), 1.0);
    }
}
