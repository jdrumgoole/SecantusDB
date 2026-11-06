//! The constant part of a WHERE, simplified as PostgreSQL 15.19's planner
//! simplifies it before planning a scan (RP-409):
//!
//! - `eval_const_expressions` folds a strict comparison over a NULL constant
//!   to NULL (`strict_fold`), a NOT of a constant to its negation, and an
//!   AND / OR over constants (`simplify_and_arguments`,
//!   `simplify_or_arguments`): a FALSE argument makes an AND false, a TRUE
//!   one makes an OR true, TRUE (AND) or FALSE (OR) arguments are dropped,
//!   and NULL ones are kept as one NULL at the end;
//! - `canonicalize_qual` (`find_duplicate_ors` with `is_check` false) then
//!   reads a NULL inside the qual's AND / OR structure as FALSE, as a WHERE
//!   does: an AND with one is false, an OR drops it.
//!
//! A WHERE that ends constant FALSE or NULL is what
//! `relation_excluded_by_constraints` finds as the relation's one restriction:
//! the relation is dummy and the plan is a `Result` whose `One-Time Filter`
//! is `false`, no scan under it. One that ends TRUE is no qual at all.
//!
//! Only the boolean structure reached from the WHERE through AND, OR and NOT
//! is simplified; a NOT over anything else is left as written (pushing it
//! onto its arms, `negate_clause`, is RP-412's).

use super::*;

/// What a qual folds to when it is a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstQual {
    True,
    False,
    Null,
}

/// The outcome of simplifying a WHERE.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Simplified {
    /// Nothing constant in it: the WHERE is left as written.
    Unchanged,
    /// The whole WHERE is the constant (`False` and `Null` both exclude
    /// every row).
    Constant(ConstQual),
    /// The WHERE with its constants simplified away.
    Rewritten(Box<pg_query::protobuf::Node>),
}

fn bool_const(n: &pg_query::protobuf::Node) -> Option<ConstQual> {
    use pg_query::protobuf::a_const::Val;
    match n.node.as_ref()? {
        N::AConst(c) if c.isnull => Some(ConstQual::Null),
        N::AConst(c) => match c.val.as_ref()? {
            Val::Boolval(b) => Some(if b.boolval {
                ConstQual::True
            } else {
                ConstQual::False
            }),
            _ => None,
        },
        // `NULL::bool` (which a folded comparison is) or `NULL::boolean`. A
        // NULL of another type is not a boolean and is left to the type
        // checks.
        N::TypeCast(tc) => {
            let to_bool = tc.type_name.as_ref().is_some_and(|t| {
                matches!(
                    t.names.last().and_then(|n| n.node.as_ref()),
                    Some(N::String(s)) if s.sval == "bool"
                ) && t.array_bounds.is_empty()
            });
            (to_bool && strict_fold::is_null_constant(n)).then_some(ConstQual::Null)
        }
        _ => None,
    }
}

fn const_node(c: ConstQual) -> pg_query::protobuf::Node {
    match c {
        ConstQual::Null => strict_fold::null_bool(),
        ConstQual::True | ConstQual::False => pg_query::protobuf::Node {
            node: Some(N::AConst(pg_query::protobuf::AConst {
                val: Some(pg_query::protobuf::a_const::Val::Boolval(
                    pg_query::protobuf::Boolean {
                        boolval: c == ConstQual::True,
                    },
                )),
                location: -1,
                ..Default::default()
            })),
        },
    }
}

fn bool_expr(op: BoolExprType, args: Vec<pg_query::protobuf::Node>) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
            boolop: op as i32,
            args,
            location: -1,
            ..Default::default()
        }))),
    }
}

/// Does a constant appear anywhere in the qual's boolean structure, or a
/// BETWEEN a NULL bound makes one: the cheap test before cloning.
fn has_constant(n: &pg_query::protobuf::Node) -> bool {
    if bool_const(n).is_some() {
        return true;
    }
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) => b.args.iter().any(has_constant),
        Some(N::NullTest(t)) => t.arg.as_deref().is_some_and(strict_fold::null_through),
        Some(N::AExpr(e)) => {
            strict_fold::null_through(n)
                || expand_between(e)
                    .is_some_and(|x| strict_fold::fold_expr(&mut x.clone()).unwrap_or(false))
        }
        _ => false,
    }
}

/// `eval_const_expressions` over the boolean structure: returns the
/// simplified node.
fn simplify(n: pg_query::protobuf::Node) -> Result<pg_query::protobuf::Node> {
    let mut n = n;
    // A strict comparison over a NULL is NULL, however deep.
    if strict_fold::null_through(&n) {
        if let Some(N::AExpr(e)) = n.node.as_ref() {
            if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprOp) {
                strict_fold::fold_expr(&mut n)?;
            }
        }
    }
    // `x IS [NOT] NULL` over an argument strictness makes NULL is a
    // constant.
    if let Some(N::NullTest(t)) = n.node.as_ref() {
        if t.arg.as_deref().is_some_and(strict_fold::null_through) {
            let is_null = t.nulltesttype == pg_query::protobuf::NullTestType::IsNull as i32;
            return Ok(const_node(if is_null {
                ConstQual::True
            } else {
                ConstQual::False
            }));
        }
        return Ok(n);
    }
    // `transformAExprBetween` makes a BETWEEN its comparisons before the
    // planner folds them: one a NULL bound folds is taken apart.
    if let Some(N::AExpr(e)) = n.node.as_ref() {
        if let Some(mut expanded) = expand_between(e) {
            if strict_fold::fold_expr(&mut expanded)? {
                return simplify(expanded);
            }
        }
        return Ok(n);
    }
    let Some(N::BoolExpr(b)) = n.node.as_ref() else {
        return Ok(n);
    };
    let op = BoolExprType::try_from(b.boolop).map_err(|e| Error::Parse(e.to_string()))?;
    let args = b
        .args
        .iter()
        .cloned()
        .map(simplify)
        .collect::<Result<Vec<_>>>()?;
    match op {
        BoolExprType::NotExpr => {
            let [arg] = args.as_slice() else {
                return Ok(n);
            };
            Ok(match bool_const(arg) {
                Some(ConstQual::True) => const_node(ConstQual::False),
                Some(ConstQual::False) => const_node(ConstQual::True),
                Some(ConstQual::Null) => const_node(ConstQual::Null),
                None => bool_expr(op, args),
            })
        }
        BoolExprType::Undefined => Ok(n),
        BoolExprType::AndExpr | BoolExprType::OrExpr => {
            let and = op == BoolExprType::AndExpr;
            // The constant that decides it, and the one each drops.
            let (decides, dropped) = if and {
                (ConstQual::False, ConstQual::True)
            } else {
                (ConstQual::True, ConstQual::False)
            };
            let mut kept = Vec::new();
            let mut have_null = false;
            let mut pending = args;
            while !pending.is_empty() {
                let arg = pending.remove(0);
                // A nested AND in an AND (OR in an OR) is flattened.
                if let Some(N::BoolExpr(inner)) = arg.node.as_ref() {
                    if inner.boolop == op as i32 {
                        let mut nested = inner.args.clone();
                        nested.append(&mut pending);
                        pending = nested;
                        continue;
                    }
                }
                match bool_const(&arg) {
                    Some(c) if c == decides => return Ok(const_node(decides)),
                    Some(c) if c == dropped => {}
                    Some(_) => have_null = true,
                    None => kept.push(arg),
                }
            }
            if kept.is_empty() {
                return Ok(const_node(if have_null {
                    ConstQual::Null
                } else {
                    dropped
                }));
            }
            if have_null {
                kept.push(const_node(ConstQual::Null));
            }
            Ok(if kept.len() == 1 {
                kept.remove(0)
            } else {
                bool_expr(op, kept)
            })
        }
    }
}

/// `find_duplicate_ors`' constant handling for a WHERE: inside the AND / OR
/// structure a NULL is FALSE.
fn canonicalize(n: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    let Some(N::BoolExpr(b)) = n.node.as_ref() else {
        return n;
    };
    let and = b.boolop == BoolExprType::AndExpr as i32;
    if !and && b.boolop != BoolExprType::OrExpr as i32 {
        return n;
    }
    let mut kept = Vec::new();
    for arg in b.args.iter().cloned().map(canonicalize) {
        match bool_const(&arg) {
            Some(ConstQual::True) if and => {}
            Some(ConstQual::True) => return const_node(ConstQual::True),
            Some(_) if and => return const_node(ConstQual::False),
            Some(_) => {}
            None => kept.push(arg),
        }
    }
    match kept.len() {
        0 => const_node(if and {
            ConstQual::True
        } else {
            ConstQual::False
        }),
        1 => kept.remove(0),
        _ => bool_expr(
            if and {
                BoolExprType::AndExpr
            } else {
                BoolExprType::OrExpr
            },
            kept,
        ),
    }
}

/// Simplify a WHERE's constants as the planner does before it plans the
/// relation's scan.
pub(crate) fn simplify_where(where_clause: &pg_query::protobuf::Node) -> Result<Simplified> {
    if !has_constant(where_clause) {
        return Ok(Simplified::Unchanged);
    }
    let out = canonicalize(simplify(where_clause.clone())?);
    Ok(match bool_const(&out) {
        Some(c) => Simplified::Constant(c),
        None if out == *where_clause => Simplified::Unchanged,
        None => Simplified::Rewritten(Box::new(out)),
    })
}

/// Is every FROM item a plain table, or a join of plain tables: the
/// relations whose scans a constant-false WHERE makes dummy.
fn plain_relations(
    n: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> bool {
    match n.node.as_ref() {
        Some(N::RangeVar(rv)) => lookup(&rv.relname).is_some(),
        Some(N::JoinExpr(j)) => {
            j.larg
                .as_deref()
                .is_some_and(|l| plain_relations(l, lookup))
                && j.rarg
                    .as_deref()
                    .is_some_and(|r| plain_relations(r, lookup))
        }
        _ => false,
    }
}

/// Simplify the constants of an EXPLAINed statement's WHERE in place: a
/// SELECT over plain tables, or an UPDATE or DELETE of one table. True when
/// the WHERE is constant FALSE or NULL, which makes the relation dummy (the
/// WHERE is then left as it is: it matches no row either way).
pub(crate) fn simplify_statement(
    query: &mut N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<bool> {
    let where_clause = match query {
        N::SelectStmt(s)
            if s.op == pg_query::protobuf::SetOperation::SetopNone as i32
                && s.with_clause.is_none()
                && !s.from_clause.is_empty()
                && s.from_clause.iter().all(|f| plain_relations(f, lookup)) =>
        {
            &mut s.where_clause
        }
        N::UpdateStmt(u)
            if u.from_clause.is_empty()
                && u.with_clause.is_none()
                && u.relation
                    .as_ref()
                    .is_some_and(|r| lookup(&r.relname).is_some()) =>
        {
            &mut u.where_clause
        }
        N::DeleteStmt(d)
            if d.using_clause.is_empty()
                && d.with_clause.is_none()
                && d.relation
                    .as_ref()
                    .is_some_and(|r| lookup(&r.relname).is_some()) =>
        {
            &mut d.where_clause
        }
        _ => return Ok(false),
    };
    let Some(w) = where_clause.as_deref() else {
        return Ok(false);
    };
    Ok(match simplify_where(w)? {
        Simplified::Unchanged => false,
        Simplified::Constant(ConstQual::True) => {
            *where_clause = None;
            false
        }
        Simplified::Constant(_) => true,
        Simplified::Rewritten(n) => {
            *where_clause = Some(n);
            false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(sql: &str) -> pg_query::protobuf::SelectStmt {
        let parsed = pg_query::parse(sql).unwrap();
        let stmt = parsed.protobuf.stmts[0].stmt.as_ref().unwrap();
        let Some(N::SelectStmt(s)) = stmt.node.as_ref() else {
            panic!("not a select")
        };
        (**s).clone()
    }

    /// The simplified WHERE as SQL, or the outcome's name.
    fn simplified(sql: &str) -> String {
        let mut sel = parsed(sql);
        match simplify_where(sel.where_clause.as_deref().unwrap()).unwrap() {
            Simplified::Unchanged => "unchanged".into(),
            Simplified::Constant(c) => format!("{c:?}"),
            Simplified::Rewritten(n) => {
                sel.where_clause = Some(n);
                let text = pg_query::protobuf::Node {
                    node: Some(N::SelectStmt(Box::new(sel))),
                }
                .deparse()
                .unwrap();
                text.split_once(" WHERE ").unwrap().1.to_string()
            }
        }
    }

    /// Each shape PostgreSQL 15.19 plans as a dummy relation, measured on a
    /// private cluster.
    #[test]
    fn a_constant_false_or_null_where() {
        for sql in [
            "select * from e where a = null",
            "select * from e where null = a",
            "select * from e where a < null",
            "select * from e where a = null::int",
            "select * from e where false",
            "select * from e where a > 5 and false",
            "select * from e where not (a = null)",
            "select * from e where a between null and 15 and b <> 'v3'",
        ] {
            let got = simplified(sql);
            assert!(got == "False" || got == "Null", "{sql}: {got}");
        }
        assert_eq!(simplified("select * from e where null"), "Null");
    }

    #[test]
    fn an_or_drops_its_null_arm_and_true_wins() {
        assert_eq!(
            simplified("select * from e where a = null or b = 'v3'"),
            "b = 'v3'"
        );
        assert_eq!(
            simplified("select * from e where (a = null or b = 'v3') and a > 5"),
            "b = 'v3' AND a > 5"
        );
        assert_eq!(simplified("select * from e where a > 5 or true"), "True");
        assert_eq!(simplified("select * from e where a > 5 and true"), "a > 5");
    }

    #[test]
    fn nothing_constant_is_left_alone() {
        for sql in [
            "select * from e where a > 5",
            "select * from e where a is null",
            "select * from e where a between 1 and 15",
            "select * from e where not (a > 5)",
            "select * from e where a is not null",
        ] {
            assert_eq!(simplified(sql), "unchanged", "{sql}");
        }
        assert_eq!(
            simplified("select * from e where (a = null) is null"),
            "True"
        );
        assert_eq!(
            simplified("select * from e where a > 1 and (a + null) is not null"),
            "False"
        );
    }
}
