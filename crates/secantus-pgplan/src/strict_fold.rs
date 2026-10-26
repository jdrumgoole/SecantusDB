//! `eval_const_expressions`' rule for a STRICT operator: one whose argument
//! is a NULL constant is NULL, folded before the statement runs, so the
//! other argument is never evaluated (RP-075). `select count(*) from t where
//! c / 0 = NULL` is 0 on PostgreSQL 15.19, not 22012: the plan is
//! `One-Time Filter: false`.
//!
//! Only the built-in operators that are strict for every overload are folded
//! (array `||` is `array_append`, which is not strict), and none that a user
//! has given a definition of their own. A non-strict test (`IS NULL`) and an
//! operand that is not a constant are left alone, so `c / 0 IS NULL` still
//! raises, as it does there.

use super::*;

/// The built-in operators every overload of which is strict.
const STRICT: &[&str] = &[
    "+", "-", "*", "/", "%", "^", "=", "<>", "!=", "<", "<=", ">", ">=",
];

/// Comparison operators: their folded NULL is a boolean.
const COMPARISON: &[&str] = &["=", "<>", "!=", "<", "<=", ">", ">="];

fn is_null_constant(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::AConst(c)) => c.isnull,
        Some(N::TypeCast(tc)) => tc.arg.as_deref().is_some_and(is_null_constant),
        _ => false,
    }
}

/// `NULL::pg_catalog.bool`, the value and type a folded comparison has.
fn null_bool() -> pg_query::protobuf::Node {
    let null = pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: true,
            ..Default::default()
        })),
    };
    pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(null)),
            type_name: Some(pg_query::protobuf::TypeName {
                names: ["pg_catalog", "bool"]
                    .iter()
                    .map(|s| pg_query::protobuf::Node {
                        node: Some(N::String(pg_query::protobuf::String {
                            sval: (*s).to_string(),
                        })),
                    })
                    .collect(),
                typemod: -1,
                ..Default::default()
            }),
            location: -1,
        }))),
    }
}

/// A strict, built-in, not user-redefined operator node: its operator.
fn strict_op(n: &pg_query::protobuf::Node) -> Option<&str> {
    let Some(N::AExpr(e)) = n.node.as_ref() else {
        return None;
    };
    if AExprKind::try_from(e.kind) != Ok(AExprKind::AexprOp)
        || e.lexpr.is_none()
        || e.rexpr.is_none()
    {
        return None;
    }
    let op = operator_name(e).ok()?;
    (STRICT.contains(&op) && !user_ops::defines(op)).then_some(op)
}

/// Is `n` NULL by strictness alone: a NULL constant, or a strict operator
/// with such an argument, however deep.
fn null_through(n: &pg_query::protobuf::Node) -> bool {
    if is_null_constant(n) {
        return true;
    }
    if strict_op(n).is_none() {
        return false;
    }
    let Some(N::AExpr(e)) = n.node.as_ref() else {
        return false;
    };
    e.lexpr.as_deref().is_some_and(null_through) || e.rexpr.as_deref().is_some_and(null_through)
}

/// Fold each COMPARISON that strictness makes NULL into `NULL::bool`. Only
/// comparisons: their type is boolean whatever the operands, where an
/// arithmetic node's type is its operands' (`pg_typeof(1 + NULL)` is
/// integer), which a bare NULL would lose. An arithmetic node with a NULL
/// operand is reached through the comparison above it (`c / 0 + NULL > 1`).
fn fold(node: &mut pg_query::protobuf::Node) -> Result<bool> {
    let mut changed = false;
    walk_expr(node, &mut |n| {
        if strict_op(n).is_some_and(|op| COMPARISON.contains(&op)) && null_through(n) {
            *n = null_bool();
            changed = true;
        }
        Ok(())
    })?;
    Ok(changed)
}

/// Fold strict comparisons over NULL constants in this SELECT's select list,
/// WHERE and HAVING (each subquery is folded when it is planned in its
/// turn). `None` when nothing changed.
pub(crate) fn fold_select(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    // Cheap test first: no NULL constant, nothing to fold.
    let has_null = crate::select_any_node(
        s,
        &|n| matches!(n, pg_query::NodeRef::AConst(c) if c.isnull),
    );
    if !has_null {
        return Ok(None);
    }
    let mut out = s.clone();
    let mut changed = false;
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            let root_is_operator = rt.val.as_deref().is_some_and(|v| strict_op(v).is_some());
            if let Some(v) = rt.val.as_deref_mut() {
                // A folded root keeps the name the operator gave it.
                if fold(v)? {
                    changed = true;
                    if root_is_operator && rt.name.is_empty() {
                        rt.name = "?column?".into();
                    }
                }
            }
        }
    }
    for clause in [
        out.where_clause.as_deref_mut(),
        out.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        changed |= fold(clause)?;
    }
    Ok(changed.then_some(out))
}
