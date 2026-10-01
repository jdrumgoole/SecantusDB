//! A WHERE over an indexed EXPRESSION reads the expression index.
//!
//! An expression index keeps each row's computed value in a hidden field
//! (`__sqlexpr_<name>`, maintained by the executor), so `lower(t) = 'x'` is
//! the field compared with `'x'` -- which the storage index serves, where the
//! expression evaluated per row scanned the table (2.4 s at 20,000 rows).
//!
//! Only an index WITHOUT a predicate is used: a partial index's field is
//! absent from the rows its predicate excludes, which a plain WHERE would
//! still have to see. A row whose value is NULL has no field either, which is
//! exactly right: `NULL = x`, `NULL < x` and `NULL <> x` are never true.

use super::*;

/// A table's usable expression indexes: `(expression SQL, hidden field)`.
/// Supplied by the executor, which can list indexes; the planner cannot.
pub type ExprIndexHook<'a> = dyn Fn(&str) -> Vec<(String, String)> + 'a;
type Hook = ExprIndexHook<'static>;

thread_local! {
    static HOOK: std::cell::Cell<Option<*const Hook>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with `hook` as the source of expression indexes.
pub fn with_expr_index_hook<R>(hook: &ExprIndexHook<'_>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const Hook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HOOK.with(|r| r.set(self.0));
        }
    }
    // SAFETY: as `with_sequence_hook` -- only the lifetime is erased, and
    // `Restore` reinstates the previous pointer before the borrow ends.
    let ptr: *const Hook =
        unsafe { std::mem::transmute::<*const ExprIndexHook<'_>, *const Hook>(hook) };
    let _restore = Restore(HOOK.with(|r| r.replace(Some(ptr))));
    f()
}

fn indexes_of(table: &str) -> Vec<(String, String)> {
    let Some(ptr) = HOOK.with(|r| r.get()) else {
        return Vec::new();
    };
    // SAFETY: installed by `with_expr_index_hook`, which outlives this call.
    let hook = unsafe { &*ptr };
    hook(table)
}

/// A constant operand: a literal, a cast of one, or a parameter.
fn is_constant(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::AConst(_)) | Some(N::ParamRef(_)) => true,
        Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(is_constant),
        Some(N::List(l)) => l.items.iter().all(is_constant),
        _ => false,
    }
}

/// `w` with every top-level comparison of an indexed expression against a
/// constant reading the expression's hidden field, and `def` with that field
/// as a column; `None` when nothing applies.
pub(crate) fn rewrite(
    w: &pg_query::protobuf::Node,
    def: &TableDef,
) -> Option<(pg_query::protobuf::Node, TableDef)> {
    if def.name.is_empty() {
        return None;
    }
    let indexes: Vec<(String, String)> = indexes_of(&def.name)
        .into_iter()
        .filter_map(|(sql, field)| Some((normalized_expression(&sql)?, field)))
        .collect();
    if indexes.is_empty() {
        return None;
    }
    let field_of = |n: &pg_query::protobuf::Node| -> Option<String> {
        if matches!(n.node.as_ref(), Some(N::ColumnRef(_))) {
            return None;
        }
        let printed = deparse_expr(n).ok()?;
        indexes
            .iter()
            .find(|(sql, _)| *sql == printed)
            .map(|(_, f)| f.clone())
    };
    let column = |field: &str| pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node(field)],
            location: -1,
        })),
    };
    let mut used: Vec<String> = Vec::new();
    let mut out = w.clone();
    let mut visit = |c: &mut pg_query::protobuf::Node| {
        let Some(N::AExpr(e)) = c.node.as_mut() else {
            return;
        };
        let kind = AExprKind::try_from(e.kind);
        let comparison = match kind {
            Ok(AExprKind::AexprOp) => matches!(
                operator_name(e),
                Ok("=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
            ),
            Ok(AExprKind::AexprIn | AExprKind::AexprBetween | AExprKind::AexprNotBetween) => true,
            _ => false,
        };
        if !comparison {
            return;
        }
        let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
            return;
        };
        if let (Some(f), true) = (field_of(l), is_constant(r)) {
            e.lexpr = Some(Box::new(column(&f)));
            used.push(f);
        } else if kind == Ok(AExprKind::AexprOp) {
            if let (true, Some(f)) = (is_constant(l), field_of(r)) {
                e.rexpr = Some(Box::new(column(&f)));
                used.push(f);
            }
        }
    };
    match out.node.as_mut() {
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
            for a in &mut b.args {
                visit(a);
            }
        }
        _ => visit(&mut out),
    }
    if used.is_empty() {
        return None;
    }
    let mut extended = def.clone();
    for (sql, field) in &indexes {
        if !used.contains(field) || extended.column(field).is_some() {
            continue;
        }
        let ty = plan_check_expression(sql, def)
            .map(|e| column_expr_type(&e).to_string())
            .ok()
            .filter(|t| !t.is_empty())?;
        extended.columns.push(Column::new(field, &ty, false));
    }
    Some((out, extended))
}
