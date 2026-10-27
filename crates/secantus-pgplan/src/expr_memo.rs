//! What PostgreSQL works out once per expression, kept with the planned
//! expression rather than worked out again for every row (RP-131).
//!
//! PostgreSQL's parse analysis resolves a cast's target type and modifier
//! once (`transformTypeCast`), and `ExecInitSubPlan` builds a hashed
//! SubPlan's table once per plan node. This evaluator walks the parse tree
//! for every row, so a `ColumnExpr::Row` carries an [`ExprMemo`]: the answers
//! that depend on a node of its own tree alone, kept by the node's address.
//!
//! Only nodes OF that tree are kept. They are collected when the memo is
//! first used, and the tree is neither changed nor dropped while the memo
//! lives (both sit in the same `ColumnExpr`), so an address names one node
//! for as long as the memo answers for it. A node the evaluator builds for
//! itself (a rewritten cast, an expanded BETWEEN) has an address outside
//! that set and is worked out as before; caching it by address would answer
//! for whatever was built there next.

use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One planned expression's memo. A clone starts empty: the memo belongs to
/// the tree it was collected from, and a cloned expression is another tree.
#[derive(Default)]
pub struct ExprMemo(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    /// The tree's nodes have been collected.
    collected: bool,
    /// A cast's `TypeName`, by address: its type name and declared typmod.
    type_names: HashMap<usize, Option<(String, i32)>>,
    /// A hashed SubPlan's call node, by address: its table.
    subplans: HashMap<usize, Option<Arc<hashed_subplan::Table>>>,
    /// A constant cast (see [`foldable`]), by address: its value, for the
    /// resolution epoch it was computed at.
    folded: HashMap<usize, Option<(u64, Bson)>>,
    /// A function call, by address: the resolution epoch at which it was
    /// found to name a function that exists for its argument types
    /// (`function_absent_in_reference` answered nothing), as parse analysis
    /// settles once per call (`ParseFuncOrColumn`).
    resolved: HashMap<usize, Option<u64>>,
    /// Every node of the tree, by address: its static type as typed without
    /// a value (`static_type(node, NULL)`), for the resolution epoch it was
    /// worked out at -- which depends on the node and the declared types
    /// alone, as parse analysis types each node once.
    types: HashMap<usize, Option<(u64, String)>>,
    /// An operator, by its `AExpr`'s address: the epoch at which its
    /// operands were found to have an operator (`array_operand_mismatch`
    /// answered nothing).
    operators: HashMap<usize, Option<u64>>,
    /// The parameter types the evaluation declares (the statement's, then
    /// the row's fields), kept to be installed again for the next row.
    declared: Vec<Option<String>>,
}

thread_local! {
    /// The memo of the expression being evaluated on this thread, with the
    /// address of its root.
    static CURRENT: std::cell::RefCell<Option<(Arc<ExprMemo>, usize)>> =
        const { std::cell::RefCell::new(None) };
}

/// Puts back the memo that was current before [`install`].
pub(crate) struct Installed(Option<(Arc<ExprMemo>, usize)>);

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Make `memo` -- the memo of the tree rooted at `root` -- the one consulted
/// until the returned guard is dropped.
pub(crate) fn install(memo: &Arc<ExprMemo>, root: &pg_query::protobuf::Node) -> Installed {
    let addr = std::ptr::from_ref(root) as usize;
    {
        let mut inner = memo
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !inner.collected {
            collect(root, &mut inner);
            inner.collected = true;
        }
    }
    Installed(CURRENT.with(|c| c.borrow_mut().replace((Arc::clone(memo), addr))))
}

fn collect(node: &pg_query::protobuf::Node, inner: &mut Inner) {
    let Some(n) = node.node.as_ref() else {
        return;
    };
    inner.types.insert(std::ptr::from_ref(node) as usize, None);
    let mut kids: Vec<&pg_query::protobuf::Node> = Vec::new();
    match n {
        N::TypeCast(tc) => {
            if foldable(node) {
                inner.folded.insert(std::ptr::from_ref(node) as usize, None);
            }
            if let Some(tn) = tc.type_name.as_ref() {
                inner
                    .type_names
                    .insert(std::ptr::from_ref(tn) as usize, None);
            }
            kids.extend(tc.arg.as_deref());
        }
        N::AExpr(e) => {
            inner
                .operators
                .insert(std::ptr::from_ref::<AExpr>(e) as usize, None);
            kids.extend(e.lexpr.as_deref());
            kids.extend(e.rexpr.as_deref());
        }
        N::FuncCall(f) => {
            inner.resolved.insert(
                std::ptr::from_ref::<pg_query::protobuf::FuncCall>(f) as usize,
                None,
            );
            if func_name(f).as_deref() == Some(hashed_subplan::NAME) {
                inner.subplans.insert(
                    std::ptr::from_ref::<pg_query::protobuf::FuncCall>(f) as usize,
                    None,
                );
            }
            kids.extend(f.args.iter());
            kids.extend(f.agg_order.iter());
            kids.extend(f.agg_filter.as_deref());
        }
        N::SortBy(sb) => kids.extend(sb.node.as_deref()),
        N::ResTarget(rt) => kids.extend(rt.val.as_deref()),
        N::BooleanTest(t) => kids.extend(t.arg.as_deref()),
        N::CollateClause(c) => kids.extend(c.arg.as_deref()),
        N::NamedArgExpr(a) => kids.extend(a.arg.as_deref()),
        N::List(l) => kids.extend(l.items.iter()),
        N::SubLink(sl) => kids.extend(sl.testexpr.as_deref()),
        N::BoolExpr(b) => kids.extend(b.args.iter()),
        N::AArrayExpr(a) => kids.extend(a.elements.iter()),
        N::RowExpr(r) => kids.extend(r.args.iter()),
        N::CoalesceExpr(c) => kids.extend(c.args.iter()),
        N::MinMaxExpr(m) => kids.extend(m.args.iter()),
        N::NullTest(t) => kids.extend(t.arg.as_deref()),
        N::AIndirection(a) => {
            kids.extend(a.arg.as_deref());
            for ind in &a.indirection {
                if let Some(N::AIndices(idx)) = ind.node.as_ref() {
                    kids.extend(idx.lidx.as_deref());
                    kids.extend(idx.uidx.as_deref());
                }
            }
        }
        N::CaseExpr(c) => {
            kids.extend(c.arg.as_deref());
            kids.extend(c.args.iter());
            kids.extend(c.defresult.as_deref());
        }
        N::CaseWhen(w) => {
            kids.extend(w.expr.as_deref());
            kids.extend(w.result.as_deref());
        }
        _ => {}
    }
    for k in kids {
        collect(k, inner);
    }
}

fn with_current<R>(f: impl FnOnce(&ExprMemo) -> Option<R>) -> Option<R> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(|(m, _)| f(m)))
}

/// A cast target's type name and declared typmod, when `tn` is a node of the
/// expression being evaluated: worked out on first use, as parse analysis
/// does once. `None` for a node outside it.
pub(crate) fn type_name(
    tn: &pg_query::protobuf::TypeName,
    compute: impl FnOnce() -> (String, i32),
) -> Option<(String, i32)> {
    let addr = std::ptr::from_ref(tn) as usize;
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.type_names.get(&addr).cloned()
    })?;
    if let Some(hit) = known {
        return Some(hit);
    }
    let value = compute();
    with_current(|m| {
        let mut inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.type_names.insert(addr, Some(value.clone()));
        Some(())
    });
    Some(value)
}

/// Whether the call `f` names no function for its argument types: `check`
/// (`function_absent_in_reference`) answers it, and when `f` is a node of
/// the expression being evaluated a call it passed is not asked again at
/// the same resolution epoch. Parse analysis resolves a call once, from the
/// arguments' types, never their values; the evaluator asked for every
/// row, and the asking evaluated each argument a second time (RP-131).
pub(crate) fn call_unresolved(
    f: &pg_query::protobuf::FuncCall,
    check: impl FnOnce() -> Option<Error>,
) -> Option<Error> {
    let addr = std::ptr::from_ref(f) as usize;
    let epoch = schemas::resolve_epoch();
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.resolved.get(&addr).copied()
    });
    if known == Some(Some(epoch)) {
        return None;
    }
    let out = check();
    if out.is_none() && known.is_some() {
        with_current(|m| {
            let mut inner =
                m.0.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.resolved.insert(addr, Some(epoch));
            Some(())
        });
    }
    out
}

/// `static_type(node, NULL)` for a node of the expression being evaluated,
/// worked out on first use and kept for the expression at the same
/// resolution epoch; `None` for a node outside it.
pub(crate) fn null_type(
    node: &pg_query::protobuf::Node,
    compute: impl FnOnce() -> String,
) -> Option<String> {
    let addr = std::ptr::from_ref(node) as usize;
    let epoch = schemas::resolve_epoch();
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.types.get(&addr).cloned()
    })?;
    if let Some((at, t)) = known {
        if at == epoch {
            return Some(t);
        }
    }
    let t = compute();
    with_current(|m| {
        let mut inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.types.insert(addr, Some((epoch, t.clone())));
        Some(())
    });
    Some(t)
}

/// Whether the operator `e` has none for its operands' types: `check`
/// (`array_operand_mismatch`) answers it, and an operator of the expression
/// being evaluated that passed is not asked again at the same epoch -- the
/// types it reads are its operands' static ones, which parse analysis
/// settles once (`make_op`).
pub(crate) fn operator_unresolved(
    e: &AExpr,
    check: impl FnOnce() -> Option<Error>,
) -> Option<Error> {
    let addr = std::ptr::from_ref(e) as usize;
    let epoch = schemas::resolve_epoch();
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.operators.get(&addr).copied()
    });
    if known == Some(Some(epoch)) {
        return None;
    }
    let out = check();
    if out.is_none() && known.is_some() {
        with_current(|m| {
            let mut inner =
                m.0.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.operators.insert(addr, Some(epoch));
            Some(())
        });
    }
    out
}

/// Install `n_params` statement parameter types and `fields`' types after
/// them as the declared parameter types, from `memo`'s copy when its
/// statement part still matches; the previous declarations are returned
/// for [`restore_declared`].
pub(crate) fn declare_row_fields(
    memo: &ExprMemo,
    n_params: usize,
    fields: &[RowField],
) -> Vec<Option<String>> {
    let mut mine = std::mem::take(
        &mut memo
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .declared,
    );
    PLAN_PARAM_TYPES.with(|t| {
        let mut current = t.borrow_mut();
        let same = mine.len() == n_params + fields.len()
            && (0..n_params).all(|i| mine[i].as_ref() == current.get(i).and_then(Option::as_ref));
        if !same {
            mine.clear();
            mine.extend((0..n_params).map(|i| current.get(i).cloned().flatten()));
            mine.extend(fields.iter().map(|(_, _, ty)| Some(ty.clone())));
        }
        std::mem::replace(&mut *current, mine)
    })
}

/// Put back the declarations [`declare_row_fields`] replaced, keeping the
/// row's for the next evaluation.
pub(crate) fn restore_declared(memo: &ExprMemo, previous: Vec<Option<String>>) {
    let mine = PLAN_PARAM_TYPES.with(|t| std::mem::replace(&mut *t.borrow_mut(), previous));
    memo.0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .declared = mine;
}

/// A hashed SubPlan's table, when `f` is a node of the expression being
/// evaluated: built on first use and kept for the expression's life, as
/// `ExecInitSubPlan` keeps it for the plan's. `None` for a node outside it,
/// whose caller builds the table for the one evaluation.
pub(crate) fn subplan_table(
    f: &pg_query::protobuf::FuncCall,
    build: impl FnOnce() -> Arc<hashed_subplan::Table>,
) -> Option<Arc<hashed_subplan::Table>> {
    let addr = std::ptr::from_ref(f) as usize;
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.subplans.get(&addr).cloned()
    })?;
    if let Some(hit) = known {
        return Some(hit);
    }
    let table = build();
    with_current(|m| {
        let mut inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.subplans.insert(addr, Some(Arc::clone(&table)));
        Some(())
    });
    Some(table)
}

/// The types a constant is cast to without reading a session setting or
/// anything but the value itself: their input and output functions are
/// pure.
fn settings_free(ty: &str) -> bool {
    let mut t = ty.to_string();
    for _ in 0..16 {
        match domains::domain_base(&t) {
            Some(base) if base != t => t = base,
            _ => break,
        }
    }
    matches!(
        t.as_str(),
        "int2"
            | "int4"
            | "int8"
            | "oid"
            | "text"
            | "varchar"
            | "name"
            | "bpchar"
            | "bool"
            | "numeric"
    ) || t == QUOTED_CHAR
}

/// Is the node a NULL literal, under any casts?
fn null_literal(node: &pg_query::protobuf::Node) -> bool {
    match node.node.as_ref() {
        Some(N::AConst(c)) => c.isnull,
        Some(N::TypeCast(tc)) => tc.arg.as_deref().is_some_and(null_literal),
        _ => false,
    }
}

/// A cast of a constant whose value is the same for every row and every
/// session setting: a literal cast to a type in [`settings_free`] (through
/// any chain of such casts), or a NULL literal cast to anything. PostgreSQL
/// folds such a cast while planning (`eval_const_expressions`); here its
/// value is kept from the first row it is evaluated for, so an error is
/// still raised where it was, and only a value is kept.
fn foldable(node: &pg_query::protobuf::Node) -> bool {
    let Some(N::TypeCast(tc)) = node.node.as_ref() else {
        return false;
    };
    let Some(arg) = tc.arg.as_deref() else {
        return false;
    };
    if null_literal(node) {
        return true;
    }
    let target_ok = tc
        .type_name
        .as_ref()
        .is_some_and(|tn| settings_free(&type_name_of_uncached(tn)));
    target_ok
        && match arg.node.as_ref() {
            Some(N::AConst(_)) => true,
            Some(N::TypeCast(_)) => foldable(arg),
            _ => false,
        }
}

/// The value of a constant cast of the expression being evaluated, kept
/// from its first evaluation (see [`foldable`]); `compute` evaluates it.
/// `None` for a node that is not one.
pub(crate) fn folded(
    node: &pg_query::protobuf::Node,
    compute: impl FnOnce() -> Result<Bson>,
) -> Option<Result<Bson>> {
    let addr = std::ptr::from_ref(node) as usize;
    let epoch = schemas::resolve_epoch();
    let known = with_current(|m| {
        let inner =
            m.0.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.folded.get(&addr).cloned()
    })?;
    if let Some((at, value)) = known {
        if at == epoch {
            return Some(Ok(value));
        }
    }
    let value = compute();
    if let Ok(v) = &value {
        with_current(|m| {
            let mut inner =
                m.0.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.folded.insert(addr, Some((epoch, v.clone())));
            Some(())
        });
    }
    Some(value)
}
