//! A string literal read as a `regclass` while the statement is analysed
//! (RP-458).
//!
//! PostgreSQL coerces an untyped literal to the type it is given as the
//! expression is transformed: `coerce_type` turns an `unknown` Const into a
//! Const of the target type by running the type's input function there and
//! then. For `regclass` that is `regclassin`, which looks the relation up, so
//! a literal naming a relation that does not exist is 42P01 at the literal
//! before the statement is planned or run, wherever it sits -- in a WHERE
//! that folds to false, a CASE arm never taken, a subquery that yields no
//! row, a CTE nothing reads. Three shapes give a literal that type:
//!
//! - an argument in a `regclass` position of the built-in a call resolves to
//!   (`nextval('s')`, `pg_relation_size('t', 'main')`), coerced by
//!   `make_fn_arguments` once `ParseFuncOrColumn` has transformed the
//!   arguments and chosen the function;
//! - an explicit cast (`'t'::regclass`, `cast('t' as regclass)`,
//!   `regclass 't'`), coerced by `transformTypeCast`;
//! - the call form `regclass('t')`, which `func_get_detail` takes as a
//!   coercion of an unknown literal rather than a call of `regclass(text)`.
//!
//! The lookup is the one the cast already makes when it runs
//! ([`crate::cast_value`]), so the error, the oid a digit string stands for
//! and the improper-name refusal are the same.
//!
//! **Order.** PostgreSQL stops at the first error its transform meets, the
//! FROM list before the clauses, each clause's expressions left to right,
//! an operator's operands before the operator. This pass walks in that
//! order (`walk`), but does not resolve every name the transform would. So
//! a literal is read only when nothing earlier in the walk could have
//! stopped PostgreSQL first: no FROM relation went unfound, and every
//! column reference met so far resolved (a reference this pass cannot
//! settle -- a function's parameter, an untyped FROM item -- counts as one
//! that might not). Otherwise the literal is left as it was, to be read
//! when it runs, and the earlier error is the planner's to report.

use super::*;

use std::cell::Cell;

thread_local! {
    /// A column reference the walk met that it could not show resolves.
    static UNSETTLED_REFERENCE: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with no unsettled reference met, restoring the flag of an
/// enclosing pass after: a pass started inside another (a statement planned
/// while one is analysed) has its own order.
pub(super) fn analysing<T>(f: impl FnOnce() -> T) -> T {
    let outer = UNSETTLED_REFERENCE.with(|u| u.replace(false));
    let out = f();
    UNSETTLED_REFERENCE.with(|u| u.set(outer));
    out
}

const SYSTEM_COLUMNS: &[&str] = &["tableoid", "ctid", "xmin", "xmax", "cmin", "cmax", "oid"];

/// Note a column reference the walk has reached: one it cannot show
/// resolves in `scope` (or a scope around it) stops literals after it from
/// being read here.
pub(super) fn note_reference(c: &pg_query::protobuf::ColumnRef, scope: &Scope) {
    if !resolves(c, scope) {
        UNSETTLED_REFERENCE.with(|u| u.set(true));
    }
}

fn resolves(c: &pg_query::protobuf::ColumnRef, scope: &Scope) -> bool {
    fn field(n: &pg_query::protobuf::Node) -> Option<Option<&str>> {
        match n.node.as_ref() {
            Some(N::String(s)) => Some(Some(s.sval.as_str())),
            Some(N::AStar(_)) => Some(None),
            _ => None,
        }
    }
    let Some(parts) = c.fields.iter().map(field).collect::<Option<Vec<_>>>() else {
        return false;
    };
    match parts.as_slice() {
        // `*`: the FROM items' columns.
        [None] => true,
        [Some(name)] => {
            SYSTEM_COLUMNS.contains(name)
                || scope.knows(name) == Some(true)
                || scope.names_item(name)
        }
        [Some(qualifier), column] => {
            let mut level = Some(scope);
            while let Some(s) = level {
                if let Some((_, def)) = s.tables.iter().find(|(a, _)| a == qualifier) {
                    return match column {
                        None => true,
                        Some(col) => SYSTEM_COLUMNS.contains(col) || def.column(col).is_some(),
                    };
                }
                level = s.parent;
            }
            false
        }
        _ => false,
    }
}

/// Can a literal be read here, with nothing before it in the walk that
/// might have been PostgreSQL's error first?
fn readable(scope: &Scope) -> bool {
    !UNSETTLED_REFERENCE.with(Cell::get) && !scope.relation_unresolved()
}

fn untyped_literal(n: &pg_query::protobuf::Node) -> Option<&pg_query::protobuf::AConst> {
    match n.node.as_ref() {
        Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) => {
            Some(c)
        }
        _ => None,
    }
}

/// `regclassin` over the literal: its error at the literal.
fn read(c: &pg_query::protobuf::AConst) -> Result<()> {
    let Some(pg_query::protobuf::a_const::Val::Sval(s)) = c.val.as_ref() else {
        return Ok(());
    };
    match crate::cast_value(bson::Bson::String(s.sval.clone()), "regclass") {
        Ok(_) => Ok(()),
        Err(e) => {
            set_error_location(c.location);
            Err(e)
        }
    }
}

/// A cast of an untyped literal to `regclass`.
pub(super) fn cast(tc: &pg_query::protobuf::TypeCast, scope: &Scope) -> Result<()> {
    let (Some(arg), Some(tn)) = (tc.arg.as_deref(), tc.type_name.as_ref()) else {
        return Ok(());
    };
    match untyped_literal(arg) {
        Some(c) if type_name_of(tn) == "regclass" && readable(scope) => read(c),
        _ => Ok(()),
    }
}

/// A call's untyped literals in the `regclass` positions of the built-in it
/// resolves to, once its arguments are transformed.
pub(super) fn arguments(f: &pg_query::protobuf::FuncCall, scope: &Scope) -> Result<()> {
    if f.agg_star || f.func_variadic || !f.args.iter().any(|a| untyped_literal(a).is_some()) {
        return Ok(());
    }
    let Some(name) = func_name(f) else {
        return Ok(());
    };
    if f.funcname.len() > 2
        || correlated::user_function_named(&name)
        || !crate::funcsig::is_builtin_function_name(&name)
        || !readable(scope)
    {
        return Ok(());
    }
    // `regclass('t')`: a type's name over one unknown literal is a coercion
    // (`func_get_detail`), not a call of `regclass(text)`.
    if name == "regclass" {
        return match f.args.as_slice() {
            [arg] => untyped_literal(arg).map_or(Ok(()), read),
            _ => Ok(()),
        };
    }
    let Some(types) = call_arg_types(f, scope) else {
        return Ok(());
    };
    let Some(params) = crate::funcsig::selected_args(&name, &types) else {
        return Ok(());
    };
    for (arg, param) in f.args.iter().zip(&params) {
        if param == "regclass" {
            if let Some(c) = untyped_literal(arg) {
                read(c)?;
            }
        }
    }
    Ok(())
}

/// The literals under `n` that [`walk`] does not reach itself, read in the
/// order the transform meets them: a call's arguments before the call.
/// Used where the walk judges something before its operands (an operator's
/// overload) and under the expressions it does not descend into.
pub(super) fn scan(n: &pg_query::protobuf::Node, scope: &Scope) -> Result<()> {
    let found = std::cell::RefCell::new(None);
    crate::expr_any(n, &|m| {
        let outcome = match m.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                note_reference(c, scope);
                Ok(())
            }
            Some(N::FuncCall(f)) => f
                .args
                .iter()
                .try_for_each(|a| scan(a, scope))
                .and_then(|()| arguments(f, scope)),
            Some(N::TypeCast(tc)) => cast(tc, scope),
            _ => Ok(()),
        };
        match outcome {
            Ok(()) => false,
            Err(e) => {
                *found.borrow_mut() = Some(e);
                true
            }
        }
    });
    found.into_inner().map_or(Ok(()), Err)
}
