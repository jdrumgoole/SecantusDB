//! PostgreSQL's `exprTypmod` (`nodeFuncs.c`) over the RAW parse tree: the
//! type modifier an output column carries in the RowDescription, and that
//! `CREATE TABLE AS` stores as `atttypmod` (RP-489, RP-493).
//!
//! PostgreSQL answers both from the PLANNED target list (the portal's and
//! the executor's tuple descriptor), so a CASE or a COALESCE is first
//! simplified as `eval_const_expressions` simplifies it, and the modifier
//! read from what is left. This server describes a statement without
//! analysing it, so the rule is applied to the shapes whose modifier
//! survives, each as `nodeFuncs.c` reads it:
//!
//! * a cast to a type with a modifier (`2.5::numeric(5,2)`) carries it --
//!   the length coercion's own modifier;
//! * a column carries its `atttypmod` (the caller's `leaf` lookup);
//! * `CASE`, `COALESCE`, `GREATEST` / `LEAST` and `ARRAY[...]` carry one
//!   only when EVERY operand has the expression's own type and the same
//!   modifier: an operand of another type is coerced to the common one,
//!   which drops its modifier. `CASE` with no `ELSE` gets a NULL default
//!   (`transformCaseExpr`), whose modifier is -1;
//! * a searched CASE drops a `WHEN false` / `WHEN null` arm and ends at a
//!   `WHEN true`, whose result becomes the default; a COALESCE drops a
//!   NULL constant and ends at a non-NULL one, which is the whole answer
//!   when it comes first (`eval_const_expressions`). Only a literal
//!   condition or argument is recognised as a constant here;
//! * `NULLIF` is its first argument's, `COLLATE` its argument's;
//! * `current_time(3)` and its siblings carry their precision.
//!
//! Anything else -- an operator, a function call, a literal, a parameter
//! -- has none (-1), as in PostgreSQL.
//!
//! Some answers come from the ANALYSED query instead, before any constant
//! is folded (measured on PostgreSQL 15.19): a prepared statement's
//! Describe (`PlanCacheComputeResultDesc`), a view's columns
//! (`DefineView`), and `CREATE TABLE AS ... WITH NO DATA`
//! (`create_ctas_nodata`). Those run under [`with_analysed_typmods`], so
//! `case when true then 'a'::varchar(3) end` is -1 there and 7 elsewhere.

use super::*;

thread_local! {
    /// Whether a modifier is read from the analysed query rather than the
    /// planned one (`with_analysed_typmods`).
    static ANALYSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` reading every modifier as parse analysis leaves it, unfolded.
pub fn with_analysed_typmods<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ANALYSED.with(|a| a.set(self.0));
        }
    }
    let _restore = Restore(ANALYSED.with(|a| a.replace(true)));
    f()
}

/// Whether modifiers are being read from the analysed query (inside
/// [`with_analysed_typmods`]): part of any cache key over a description.
pub fn analysed_typmods() -> bool {
    ANALYSED.with(|a| a.get())
}

/// What answers a column's `(type, atttypmod)`: called on a `ColumnRef`
/// and on a `ParamRef` (a row expression's rewritten column). A caller with
/// no FROM passes one that knows none.
pub(crate) type Leaf<'a> = &'a dyn Fn(&pg_query::protobuf::Node) -> Option<(String, i32)>;

/// The modifier `node` carries in a planned target list (in an analysed
/// one inside [`with_analysed_typmods`]), or -1.
pub(crate) fn raw_expr_typmod(node: &pg_query::protobuf::Node, leaf: Leaf) -> i32 {
    let fold = !analysed_typmods();
    typed(node, &Ctx { leaf, fold }).map_or(-1, |(_, m)| m)
}

/// `(type, modifier)` of `node` as parse analysis leaves it, before any
/// constant is folded: what a VALUES list's column modifier is computed
/// from (`transformValuesClause`), or `None` for no modifier.
pub(crate) fn analysed_expr_typed(node: &pg_query::protobuf::Node) -> Option<(String, i32)> {
    typed(
        node,
        &Ctx {
            leaf: &|_| None,
            fold: false,
        },
    )
}

/// How an expression is read: its columns' lookup, and whether CASE and
/// COALESCE are simplified first (a planned target list) or not (parse
/// analysis).
struct Ctx<'a> {
    leaf: Leaf<'a>,
    fold: bool,
}

/// `(type, modifier)` for an expression that carries a modifier (>= 0);
/// `None` for one that carries none. The type is what decides whether a
/// CASE's or a COALESCE's operands agree (`exprType(e) != casetype`).
fn typed(node: &pg_query::protobuf::Node, cx: &Ctx) -> Option<(String, i32)> {
    let found = match node.node.as_ref()? {
        N::TypeCast(tc) => {
            let tn = tc.type_name.as_ref()?;
            Some((type_name_of(tn), declared_typmod(tn)))
        }
        N::ColumnRef(_) | N::ParamRef(_) => (cx.leaf)(node),
        N::CollateClause(c) => typed(c.arg.as_deref()?, cx),
        N::AExpr(e) if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif) => {
            typed(e.lexpr.as_deref()?, cx)
        }
        N::CaseExpr(c) => case_typed(c, cx),
        N::CoalesceExpr(c) => coalesce_typed(&c.args, cx),
        N::MinMaxExpr(m) => all_agree(m.args.iter(), cx),
        N::AArrayExpr(a) => {
            // An element of array type makes a multi-dimensional array of
            // that same type; any other element type `T` makes `T[]`. The
            // modifier is the elements' common one either way.
            let (ty, m) = all_agree(a.elements.iter(), cx)?;
            let ty = if ty.ends_with("[]") {
                ty
            } else {
                format!("{ty}[]")
            };
            Some((ty, m))
        }
        N::SqlvalueFunction(f) if f.typmod >= 0 => {
            Some((sql_value_function_type(f).to_string(), f.typmod))
        }
        _ => None,
    };
    found.filter(|(_, m)| *m >= 0)
}

/// A literal constant, as `eval_const_expressions` would leave it: a bare
/// literal, or one under casts (which fold into it).
#[derive(PartialEq)]
enum Literal {
    Null,
    True,
    False,
    Other,
}

fn literal(node: &pg_query::protobuf::Node) -> Option<Literal> {
    use pg_query::protobuf::a_const::Val;
    match node.node.as_ref()? {
        N::AConst(c) if c.isnull => Some(Literal::Null),
        N::AConst(c) => Some(match c.val.as_ref() {
            Some(Val::Boolval(b)) if b.boolval => Literal::True,
            Some(Val::Boolval(_)) => Literal::False,
            _ => Literal::Other,
        }),
        // A cast of a literal is a constant, NULL when the literal is; its
        // truth is not read through the cast (`'t'::bool` stays unknown).
        N::TypeCast(tc) => match literal(tc.arg.as_deref()?)? {
            Literal::Null => Some(Literal::Null),
            _ => Some(Literal::Other),
        },
        _ => None,
    }
}

/// A CASE after `eval_const_expressions`: a constantly false or NULL arm is
/// dropped, a constantly true one ends the list as the new default, and a
/// CASE with no arm left is its default.
fn case_typed(c: &pg_query::protobuf::CaseExpr, cx: &Ctx) -> Option<(String, i32)> {
    let mut kept = Vec::new();
    let mut default = c.defresult.as_deref();
    for w in &c.args {
        let Some(N::CaseWhen(w)) = w.node.as_ref() else {
            return None;
        };
        // A simple CASE (`case x when 1 ...`) compares; its conditions are
        // not literals, so every arm is kept.
        let cond = if c.arg.is_some() || !cx.fold {
            None
        } else {
            w.expr.as_deref().and_then(literal)
        };
        match cond {
            Some(Literal::Null | Literal::False) => continue,
            Some(Literal::True) => {
                default = w.result.as_deref();
                break;
            }
            _ => kept.push(w.result.as_deref()?),
        }
    }
    // No ELSE is a NULL default, whose modifier is -1.
    let default = typed(default?, cx)?;
    agree(default, kept.into_iter(), cx)
}

/// A COALESCE after `eval_const_expressions`: NULL constants dropped, and
/// the list ended at the first non-NULL constant -- the whole answer when
/// nothing comes before it.
fn coalesce_typed(args: &[pg_query::protobuf::Node], cx: &Ctx) -> Option<(String, i32)> {
    if !cx.fold {
        return all_agree(args.iter(), cx);
    }
    let mut kept = Vec::new();
    for a in args {
        match literal(a) {
            Some(Literal::Null) => continue,
            Some(_) => {
                if kept.is_empty() {
                    return typed(a, cx);
                }
                kept.push(a);
                break;
            }
            None => kept.push(a),
        }
    }
    all_agree(kept.into_iter(), cx)
}

/// Every one of `args` has the same type and modifier: that pair.
fn all_agree<'a>(
    mut args: impl Iterator<Item = &'a pg_query::protobuf::Node>,
    cx: &Ctx,
) -> Option<(String, i32)> {
    let first = typed(args.next()?, cx)?;
    agree(first, args, cx)
}

fn agree<'a>(
    first: (String, i32),
    rest: impl Iterator<Item = &'a pg_query::protobuf::Node>,
    cx: &Ctx,
) -> Option<(String, i32)> {
    for node in rest {
        if typed(node, cx)? != first {
            return None;
        }
    }
    Some(first)
}

/// `select_common_typmod` for a set operation's column: the sides' shared
/// modifier when both already have the column's common type, else -1 (a
/// side of another type is coerced, which drops its modifier).
pub(crate) fn common_typmod(common: &str, sides: &[(&str, i32)]) -> i32 {
    let Some(&(_, first)) = sides.first() else {
        return -1;
    };
    if sides.iter().all(|&(ty, m)| ty == common && m == first) {
        first
    } else {
        -1
    }
}

/// The modifier of a row expression's column (`ColumnExpr::Row`), whose
/// column references were rewritten to parameters numbered past the
/// statement's own (`params.len() + 1 + i` is `fields[i]`); `def` answers
/// each field's `atttypmod`.
pub(crate) fn row_expr_typmod(
    expr: &pg_query::protobuf::Node,
    fields: &[RowField],
    params: usize,
    def: &TableDef,
) -> i32 {
    let leaf = |n: &pg_query::protobuf::Node| -> Option<(String, i32)> {
        let Some(N::ParamRef(p)) = n.node.as_ref() else {
            return None;
        };
        let i = usize::try_from(p.number).ok()?.checked_sub(params + 1)?;
        let (name, field, ty) = fields.get(i)?;
        let c = def
            .columns
            .iter()
            .find(|c| c.field() == *field)
            .or_else(|| def.column(field))
            .or_else(|| def.column(name))?;
        Some((ty.clone(), c.typmod))
    };
    raw_expr_typmod(expr, &leaf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first select-list expression of `sql`.
    fn target(sql: &str) -> pg_query::protobuf::Node {
        let parsed = pg_query::parse(sql).expect("parses");
        let stmt = parsed.protobuf.stmts[0].stmt.as_ref().expect("a statement");
        let Some(N::SelectStmt(s)) = stmt.node.as_ref() else {
            panic!("not a SELECT");
        };
        let Some(N::ResTarget(rt)) = s.target_list[0].node.as_ref() else {
            panic!("not a target");
        };
        *rt.val.clone().expect("a value")
    }

    fn planned(sql: &str) -> i32 {
        raw_expr_typmod(&target(sql), &|_| None)
    }

    fn analysed(sql: &str) -> i32 {
        with_analysed_typmods(|| raw_expr_typmod(&target(sql), &|_| None))
    }

    // Each modifier is PostgreSQL 15.19's (a private cluster), RP-493.
    #[test]
    fn operands_that_share_a_modifier_keep_it() {
        for (sql, want) in [
            ("select 2.5::numeric(5,2)", 327686),
            ("select 'a'::varchar(3)", 7),
            (
                "select case when x then 'a'::varchar(3) else 'b'::varchar(3) end",
                7,
            ),
            (
                "select case when x then 'a'::varchar(3) else 'b'::varchar(4) end",
                -1,
            ),
            ("select case when x then 'a'::varchar(3) else 'b' end", -1),
            ("select case when x then 'a'::varchar(3) end", -1),
            ("select coalesce(x::numeric(5,2), y::numeric(5,2))", 327686),
            ("select coalesce(x::numeric(5,2), y::numeric(6,2))", -1),
            ("select greatest(x::numeric(5,2), y::numeric(5,2))", 327686),
            ("select nullif(x::numeric(5,2), 2)", 327686),
            ("select array[1.5::numeric(5,2)]", 327686),
            ("select array[array[1.5::numeric(5,2)]]", 327686),
            ("select array[1.5::numeric(5,2), 2]", -1),
            ("select 2.5::numeric(5,2) + 1", -1),
            ("select current_time(3)", 3),
            ("select current_timestamp", -1),
        ] {
            assert_eq!(planned(sql), want, "{sql}");
            assert_eq!(analysed(sql), want, "{sql}");
        }
    }

    #[test]
    fn a_literal_condition_or_argument_folds_only_when_planned() {
        for (sql, planned_want) in [
            ("select case when true then 'a'::varchar(3) end", 7),
            ("select case when false then 'a'::varchar(4) else 'b'::varchar(3) end", 7),
            ("select case when null then 'a'::varchar(4) else 'b'::varchar(3) end", 7),
            ("select case when x then 'a'::varchar(3) when true then 'b'::varchar(3) else 'c' end", 7),
            ("select coalesce(2.5::numeric(5,2), 1::numeric(6,2))", 327686),
            ("select coalesce(null, 2.5::numeric(5,2))", 327686),
            ("select coalesce(x::numeric(5,2), 1::numeric(5,2), y::numeric(6,2))", 327686),
        ] {
            assert_eq!(planned(sql), planned_want, "{sql}");
            assert_eq!(analysed(sql), -1, "{sql}");
        }
        // The mode is the caller's only for the call.
        assert_eq!(planned("select case when true then 'a'::varchar(3) end"), 7);
        assert!(!analysed_typmods());
    }

    #[test]
    fn a_set_operations_column_keeps_the_sides_common_modifier() {
        assert_eq!(
            common_typmod("numeric", &[("numeric", 327686), ("numeric", 327686)]),
            327686
        );
        assert_eq!(
            common_typmod("numeric", &[("numeric", 327686), ("numeric", 393222)]),
            -1
        );
        assert_eq!(
            common_typmod("numeric", &[("numeric", 327686), ("int4", -1)]),
            -1
        );
        assert_eq!(
            common_typmod("varchar", &[("varchar", 7), ("unknown", -1)]),
            -1
        );
    }
}
