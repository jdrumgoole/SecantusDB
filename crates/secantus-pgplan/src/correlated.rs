//! Correlated subqueries: a subquery whose value depends on the OUTER row.
//!
//! An uncorrelated subquery is run once at plan time and replaced by what it
//! returned (`resolve_one_sublink`). A correlated one has a different value
//! per outer row, so it is replaced instead by a call this module evaluates
//! PER ROW: its references to the outer query become `$N` parameters of the
//! stored inner SQL, and the outer query passes those columns as the call's
//! arguments. Everything around it -- a residual WHERE, a select-list
//! expression, a join -- already evaluates expressions per row, so the call
//! needs no new plan node.
//!
//! The inner query is planned and run by the EXECUTOR's runner, installed for
//! the duration of a statement's execution (`with_correlated_runner`); the
//! planner alone cannot read storage. Results are memoised by the parameter
//! values, so an outer column with few distinct values costs few runs.
//!
//! That is O(distinct outer values) plans and scans -- correct first. The
//! semi-join rewrite for `EXISTS` / `IN` that PostgreSQL itself does is the
//! better plan and is not done here.

use super::*;
use pg_query::protobuf::a_const::Val;

/// The internal function a correlated subquery becomes. A unit separator
/// keeps it out of any name a user can write.
pub(crate) const CORRELATED: &str = "\u{1f}correlated";

/// Plans and runs a correlated subquery's SQL with its bound values.
pub type CorrelatedRunner<'a> = dyn Fn(&str, &[Bson]) -> Result<Vec<Vec<Bson>>> + 'a;
type Runner = CorrelatedRunner<'static>;

thread_local! {
    static RUNNER: std::cell::Cell<Option<*const Runner>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with `runner` as the way to run a correlated subquery's SQL. The
/// runner is only reachable while `f` runs, which is what makes the raw
/// pointer sound: it never outlives the borrow it was taken from.
pub fn with_correlated_runner<R>(runner: &CorrelatedRunner<'_>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const Runner>);
    impl Drop for Restore {
        fn drop(&mut self) {
            RUNNER.with(|r| r.set(self.0));
        }
    }
    // SAFETY: only the lifetime is erased; `Restore` puts the previous
    // pointer back before `runner`'s borrow ends, on every exit from `f`.
    let ptr: *const Runner = unsafe {
        std::mem::transmute::<
            *const (dyn Fn(&str, &[Bson]) -> Result<Vec<Vec<Bson>>> + '_),
            *const Runner,
        >(runner)
    };
    let _restore = Restore(RUNNER.with(|r| r.replace(Some(ptr))));
    // A semi-join index is a snapshot of one statement's view of the data:
    // it must not outlive the runner it was built through.
    let _decor = semijoin_hash::Scope::enter();
    f()
}

fn run(sql: &str, params: &[Bson]) -> Result<Vec<Vec<Bson>>> {
    if SUPPRESSED.with(|s| s.get()) {
        return Err(Error::Unsupported(
            "a subquery evaluated for its type".into(),
        ));
    }
    if let Some(rows) = semijoin_hash::lookup(sql, params, run_direct)? {
        return Ok(rows);
    }
    run_direct(sql, params)
}

fn run_direct(sql: &str, params: &[Bson]) -> Result<Vec<Vec<Bson>>> {
    match RUNNER.with(|r| r.get()) {
        // SAFETY: set only inside `with_correlated_runner`, whose borrow is
        // still live while the pointer is installed.
        Some(runner) => semijoin_hash::as_subquery(|| unsafe { (*runner)(sql, params) }),
        None => Err(Error::Unsupported(
            "a correlated subquery evaluated outside a statement's execution".into(),
        )),
    }
}

fn int_const(v: i32) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(Val::Ival(pg_query::protobuf::Integer { ival: v })),
        })),
    }
}

fn str_const(v: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(Val::Sval(pg_query::protobuf::String {
                sval: v.to_string(),
            })),
        })),
    }
}

fn null_const() -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: true,
            location: -1,
            val: None,
        })),
    }
}

/// Visit every expression node of a SELECT at every level -- its clauses,
/// its JOIN conditions, its FROM-subqueries, its CTEs, the sides of a set
/// operation, and the bodies of the subqueries inside its expressions.
pub(crate) fn walk_select(
    s: &mut pg_query::protobuf::SelectStmt,
    visit: &mut dyn FnMut(&mut pg_query::protobuf::Node) -> Result<()>,
) -> Result<()> {
    let expr = |n: &mut pg_query::protobuf::Node,
                visit: &mut dyn FnMut(&mut pg_query::protobuf::Node) -> Result<()>|
     -> Result<()> {
        walk_expr(n, &mut |node| {
            visit(node)?;
            if let Some(N::SubLink(sl)) = node.node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    walk_select(body, visit)?;
                }
            }
            Ok(())
        })
    };
    for n in s
        .target_list
        .iter_mut()
        .chain(s.group_clause.iter_mut())
        .chain(s.sort_clause.iter_mut())
        .chain(s.distinct_clause.iter_mut())
        .chain(s.window_clause.iter_mut())
    {
        expr(n, visit)?;
    }
    for n in [
        s.where_clause.as_deref_mut(),
        s.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        expr(n, visit)?;
    }
    for item in &mut s.from_clause {
        walk_from(item, visit)?;
    }
    for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        walk_select(side, visit)?;
    }
    if let Some(with) = s.with_clause.as_mut() {
        for cte in &mut with.ctes {
            if let Some(N::CommonTableExpr(c)) = cte.node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    c.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    walk_select(body, visit)?;
                }
            }
        }
    }
    Ok(())
}

fn walk_from(
    item: &mut pg_query::protobuf::Node,
    visit: &mut dyn FnMut(&mut pg_query::protobuf::Node) -> Result<()>,
) -> Result<()> {
    match item.node.as_mut() {
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                walk_from(side, visit)?;
            }
            if let Some(q) = j.quals.as_deref_mut() {
                walk_expr(q, visit)?;
            }
            Ok(())
        }
        Some(N::RangeSubselect(rs)) => {
            match rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut()) {
                Some(N::SelectStmt(body)) => walk_select(body, visit),
                _ => Ok(()),
            }
        }
        // A function in FROM: its argument expressions can read columns (of
        // an enclosing query, or -- LATERAL -- of the items to its left).
        Some(N::RangeFunction(rf)) => {
            for f in &mut rf.functions {
                if let Some(N::List(l)) = f.node.as_mut() {
                    for item in &mut l.items {
                        walk_expr(item, visit)?;
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Every name the FROM items of `s` can be addressed by, at every level:
/// its own FROM (and the FROM of each derived table, join side, CTE and set
/// operation arm inside it), and the FROM of every subquery in its
/// expressions. A name found only one level down is still the subquery's
/// own, never an outer reference: missing the derived tables made
/// `(SELECT r.id FROM t r WHERE ...)` read `r.id` from the OUTER query.
pub(crate) fn inner_names(s: &pg_query::protobuf::SelectStmt) -> Vec<String> {
    fn from_item(item: &pg_query::protobuf::Node, names: &mut Vec<String>) {
        // An aliased relation goes by its alias ONLY, so an inner `t r`
        // does not hide an outer `t`.
        match item.node.as_ref() {
            Some(N::RangeVar(r)) => names.push(
                r.alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| r.relname.clone()),
            ),
            Some(N::JoinExpr(_)) => {}
            _ => collect_from_names(item, names),
        }
        match item.node.as_ref() {
            Some(N::RangeSubselect(rs)) => {
                if let Some(N::SelectStmt(body)) =
                    rs.subquery.as_deref().and_then(|q| q.node.as_ref())
                {
                    select(body, names);
                }
            }
            Some(N::JoinExpr(j)) => {
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    from_item(side, names);
                }
            }
            _ => {}
        }
    }
    fn select(s: &pg_query::protobuf::SelectStmt, names: &mut Vec<String>) {
        for item in &s.from_clause {
            from_item(item, names);
        }
        for side in [s.larg.as_deref(), s.rarg.as_deref()].into_iter().flatten() {
            select(side, names);
        }
        if let Some(with) = s.with_clause.as_ref() {
            for cte in &with.ctes {
                if let Some(N::CommonTableExpr(c)) = cte.node.as_ref() {
                    names.push(c.ctename.clone());
                    if let Some(N::SelectStmt(body)) =
                        c.ctequery.as_deref().and_then(|q| q.node.as_ref())
                    {
                        select(body, names);
                    }
                }
            }
        }
        let mut copy = s.clone();
        let _ = walk_select(&mut copy, &mut |n| {
            if let Some(N::SubLink(sl)) = n.node.as_ref() {
                if let Some(N::SelectStmt(body)) =
                    sl.subselect.as_deref().and_then(|q| q.node.as_ref())
                {
                    for item in &body.from_clause {
                        from_item(item, names);
                    }
                }
            }
            Ok(())
        });
    }
    let mut names = Vec::new();
    select(s, &mut names);
    names
}

fn parts_of(c: &pg_query::protobuf::ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// Replace every reference in `outer_refs` with its `$N`.
fn substitute(
    s: &pg_query::protobuf::SelectStmt,
    outer_refs: &[Vec<String>],
    first_param: usize,
) -> pg_query::protobuf::SelectStmt {
    let mut out = s.clone();
    let _ = walk_select(&mut out, &mut |n| {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        let Some(parts) = parts_of(c) else {
            return Ok(());
        };
        if let Some(i) = outer_refs.iter().position(|r| *r == parts) {
            n.node = Some(N::ParamRef(pg_query::protobuf::ParamRef {
                number: i32::try_from(first_param + i).unwrap_or(i32::MAX),
                location: c.location,
            }));
        }
        Ok(())
    });
    out
}

/// A correlated subquery, as the per-row call that stands in for it.
///
/// `inner` has had its own uncorrelated subqueries resolved already; `outer`
/// is the column names the enclosing query's FROM exposes, which is what
/// tells a correlation from a typo for an UNQUALIFIED reference.
pub(crate) fn correlate(
    sl: &pg_query::protobuf::SubLink,
    inner: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    outer: &[String],
) -> Result<pg_query::protobuf::Node> {
    let kind = SubLinkType::try_from(sl.sub_link_type)
        .map_err(|_| Error::Unsupported("this subquery form".into()))?;
    if !matches!(
        kind,
        SubLinkType::ExistsSublink
            | SubLinkType::ExprSublink
            | SubLinkType::ArraySublink
            | SubLinkType::AnySublink
            | SubLinkType::AllSublink
    ) {
        return Err(Error::Unsupported("this correlated subquery form".into()));
    }
    // Qualified references whose qualifier names nothing inside.
    let names = inner_names(inner);
    let mut outer_refs: Vec<Vec<String>> = Vec::new();
    let mut probe = inner.clone();
    walk_select(&mut probe, &mut |n| {
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            if let Some(parts) = parts_of(c) {
                if parts.len() >= 2
                    && !names.contains(&parts[parts.len() - 2])
                    && !outer_refs.contains(&parts)
                {
                    outer_refs.push(parts);
                }
            }
        }
        Ok(())
    })?;
    // Unqualified ones, found by planning: a name the subquery cannot resolve
    // that the outer query has is a correlation; anything else is a typo.
    let first = params.len() + 1;
    let (planned, body) = loop {
        let body = substitute(inner, &outer_refs, first);
        let mut probe_params = params.to_vec();
        probe_params.extend(std::iter::repeat_n(Bson::Null, outer_refs.len()));
        match plan_select(&body, lookup, &probe_params) {
            Ok(p) => break (p, body),
            Err(Error::UndefinedColumn(name))
                if outer.contains(&name) && !outer_refs.contains(&vec![name.clone()]) =>
            {
                outer_refs.push(vec![name]);
            }
            Err(e) => return Err(e),
        }
    };
    let result_type = match kind {
        SubLinkType::ExistsSublink | SubLinkType::AnySublink | SubLinkType::AllSublink => {
            "bool".to_string()
        }
        _ => {
            let def = sub_plan_def(&planned, lookup)?;
            let first = def
                .columns
                .first()
                .map(|c| c.pg_type.clone())
                .unwrap_or_else(|| "text".into());
            if kind == SubLinkType::ArraySublink {
                format!("{first}[]")
            } else {
                first
            }
        }
    };
    let sql = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(body))),
    }
    .deparse()
    .map_err(|e| Error::Parse(e.to_string()))?;
    let op = sl
        .oper_name
        .first()
        .and_then(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "=".to_string());
    let mut args = vec![
        int_const(kind as i32),
        str_const(&sql),
        str_const(&result_type),
        str_const(&op),
        sl.testexpr.as_deref().cloned().unwrap_or_else(null_const),
        int_const(i32::try_from(params.len()).unwrap_or(i32::MAX)),
    ];
    // The statement's own parameters travel as arguments too: the inner SQL
    // numbers them `$1..$n`, exactly as the outer statement bound them.
    for i in 1..=params.len() {
        args.push(pg_query::protobuf::Node {
            node: Some(N::ParamRef(pg_query::protobuf::ParamRef {
                number: i32::try_from(i).unwrap_or(i32::MAX),
                location: -1,
            })),
        });
    }
    for parts in &outer_refs {
        args.push(pg_query::protobuf::Node {
            node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                fields: parts
                    .iter()
                    .map(|p| pg_query::protobuf::Node {
                        node: Some(N::String(pg_query::protobuf::String { sval: p.clone() })),
                    })
                    .collect(),
                location: sl.location,
            })),
        });
    }
    Ok(pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![string_node(CORRELATED)],
            args,
            // Deparsed when a correlated subquery nests inside another's
            // SQL, and libpg_query asserts on an Undefined enum there.
            funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
            location: sl.location,
            ..Default::default()
        }))),
    })
}

fn is_correlated(f: &pg_query::protobuf::FuncCall) -> bool {
    matches!(
        f.funcname.as_slice(),
        [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == CORRELATED)
    )
}

/// Does `node` hold a correlated subquery's per-row call anywhere?
pub(crate) fn has_correlated_call(node: &pg_query::protobuf::Node) -> bool {
    let mut copy = node.clone();
    let mut found = false;
    let _ = walk_expr(&mut copy, &mut |n| {
        if let Some(N::FuncCall(f)) = n.node.as_ref() {
            found |= is_correlated(f);
        }
        Ok(())
    });
    found
}

/// The static type of a correlated call, when `f` is one.
pub(crate) fn correlated_type(f: &pg_query::protobuf::FuncCall) -> Option<String> {
    if !is_correlated(f) {
        return None;
    }
    match f.args.get(2)?.node.as_ref()? {
        N::AConst(pg_query::protobuf::AConst {
            val: Some(Val::Sval(s)),
            ..
        }) => Some(s.sval.clone()),
        _ => None,
    }
}

/// Evaluate a correlated call over the row its arguments were read from, or
/// `None` when `f` is not one.
pub(crate) fn eval_correlated(
    f: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Option<Result<Bson>> {
    if !is_correlated(f) {
        return None;
    }
    Some(eval(f, params))
}

fn eval(f: &pg_query::protobuf::FuncCall, params: &[Bson]) -> Result<Bson> {
    let arg = |i: usize| -> Result<&pg_query::protobuf::Node> {
        f.args
            .get(i)
            .ok_or_else(|| Error::Internal("a malformed correlated subquery".into()))
    };
    let text = |i: usize| -> Result<String> {
        match const_value(arg(i)?, params)? {
            Bson::String(s) => Ok(s),
            _ => Err(Error::Internal("a malformed correlated subquery".into())),
        }
    };
    let int = |i: usize| -> Result<i64> {
        match const_value(arg(i)?, params)? {
            Bson::Int32(v) => Ok(i64::from(v)),
            Bson::Int64(v) => Ok(v),
            _ => Err(Error::Internal("a malformed correlated subquery".into())),
        }
    };
    let kind = SubLinkType::try_from(i32::try_from(int(0)?).unwrap_or(0))
        .map_err(|_| Error::Internal("a malformed correlated subquery".into()))?;
    let sql = text(1)?;
    let op = text(3)?;
    let n_params = usize::try_from(int(5)?).unwrap_or(0);
    let mut inner_params = Vec::new();
    for i in 6..f.args.len() {
        inner_params.push(const_value(&f.args[i], params)?);
    }
    debug_assert!(inner_params.len() >= n_params);
    let rows = run(&sql, &inner_params)?;
    let first = |r: &Vec<Bson>| r.first().cloned().unwrap_or(Bson::Null);
    match kind {
        SubLinkType::ExistsSublink => Ok(Bson::Boolean(!rows.is_empty())),
        SubLinkType::ExprSublink => {
            if rows.len() > 1 {
                return Err(Error::CardinalityViolation(
                    "more than one row returned by a subquery used as an expression".into(),
                ));
            }
            Ok(rows.first().map_or(Bson::Null, first))
        }
        SubLinkType::ArraySublink => Ok(Bson::Array(rows.iter().map(first).collect())),
        SubLinkType::AnySublink | SubLinkType::AllSublink => {
            // `x op ANY (values)` over what this row's subquery returned: the
            // constant evaluator already has ANY / ALL's three-valued rules.
            let test = const_value(arg(4)?, params)?;
            let values = Bson::Array(rows.iter().map(first).collect());
            eval_scalar_array_const(&op, test, values, kind == SubLinkType::AnySublink)
        }
        _ => Err(Error::Unsupported("this correlated subquery form".into())),
    }
}

/// Does this computed column contain a correlated subquery? Such a column
/// has to be evaluated where the executor's runner is installed, not lazily
/// inside a row stream that outlives it.
pub fn has_correlated(expr: &ColumnExpr) -> bool {
    let ColumnExpr::Row { expr, .. } = expr else {
        return false;
    };
    let mut node = (**expr).clone();
    let mut found = false;
    let _ = walk_expr(&mut node, &mut |n| {
        if let Some(N::FuncCall(f)) = n.node.as_ref() {
            // A correlated subquery, a user-defined function and a sequence
            // function all run through the EXECUTOR's hooks.
            found |= is_correlated(f)
                || func_name(f).is_some_and(|name| {
                    SEQUENCE_FUNCTIONS.contains(&name.as_str())
                        || user_function_for(&name, &f.args).is_some()
                });
        }
        // So does a cast that may take a user cast's function.
        if let Some(N::TypeCast(tc)) = n.node.as_ref() {
            found |= tc
                .type_name
                .as_ref()
                .is_some_and(|t| crate::user_casts::casts_to(&crate::type_name_of(t)));
        }
        Ok(())
    });
    found
}

/// Is `name` a user-defined function at SOME arity? A call at another arity
/// is then PostgreSQL's 42883, not an unimplemented built-in.
pub(crate) fn user_function_named(name: &str) -> bool {
    USER_FUNCTIONS.with(|f| f.borrow().iter().any(|u| u.name == name))
}

/// Runs a sequence function -- `nextval` / `currval` / `setval` / `lastval`
/// -- against the store. Supplied by the executor for the same reason as the
/// correlated runner: the planner cannot write.
pub type SequenceHook<'a> = dyn Fn(&str, &[Bson]) -> Result<Bson> + 'a;
type Hook = SequenceHook<'static>;

thread_local! {
    static SEQUENCE_HOOK: std::cell::Cell<Option<*const Hook>> = const { std::cell::Cell::new(None) };
}

/// Run `f` able to call sequence functions anywhere in an expression -- in
/// `VALUES`, in arithmetic, in an UPDATE's SET -- not only as a bare target.
pub fn with_sequence_hook<R>(hook: &SequenceHook<'_>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const Hook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SEQUENCE_HOOK.with(|r| r.set(self.0));
        }
    }
    // SAFETY: as `with_correlated_runner` -- only the lifetime is erased, and
    // `Restore` reinstates the previous pointer before the borrow ends.
    let ptr: *const Hook =
        unsafe { std::mem::transmute::<*const SequenceHook<'_>, *const Hook>(hook) };
    let _restore = Restore(SEQUENCE_HOOK.with(|r| r.replace(Some(ptr))));
    f()
}

/// The sequence functions.
pub(crate) const SEQUENCE_FUNCTIONS: &[&str] = &[
    // `pg_trgm`'s `set_limit` writes the session's threshold.
    "set_limit",
    "nextval",
    "currval",
    "setval",
    "lastval",
    // The storage-size functions ride the same executor hook: they read the
    // store, which the planner cannot.
    "pg_relation_size",
    "pg_total_relation_size",
    "pg_table_size",
    "pg_indexes_size",
    "pg_database_size",
    // Advisory locks: session state the executor keeps.
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_shared",
    "pg_advisory_unlock_all",
    "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared",
    "pg_try_advisory_xact_lock",
    "pg_try_advisory_xact_lock_shared",
    // The role graph lives in the executor's catalog.
    "pg_has_role",
    // Large objects live in the store.
    "lo_creat",
    "lo_create",
    "lo_unlink",
    "lo_open",
    "lo_close",
    "loread",
    "lowrite",
    "lo_lseek",
    "lo_lseek64",
    "lo_tell",
    "lo_tell64",
    "lo_truncate",
    "lo_truncate64",
    "lo_get",
    "lo_put",
    "lo_from_bytea",
];

/// The result type of an executor-answered function other than the
/// sequence ones.
pub fn executor_function_type(name: &str) -> Option<&'static str> {
    Some(match name {
        n if n.starts_with("pg_") && n.ends_with("_size") => "int8",
        n if n.starts_with("pg_try_advisory")
            || n.starts_with("pg_advisory_unlock") && n != "pg_advisory_unlock_all" =>
        {
            "bool"
        }
        n if n.contains("advisory") => "void",
        "pg_has_role" => "bool",
        "set_limit" => "float4",
        "lo_creat" | "lo_create" | "lo_from_bytea" => "oid",
        "lo_unlink" | "lo_open" | "lo_close" | "lowrite" | "lo_lseek" | "lo_tell"
        | "lo_truncate" | "lo_truncate64" => "int4",
        "lo_lseek64" | "lo_tell64" => "int8",
        "loread" | "lo_get" => "bytea",
        "lo_put" => "void",
        _ => return None,
    })
}

/// Call a sequence function. With no hook installed -- a Describe, or any
/// plan that will not execute -- the call must NOT advance anything, and its
/// value is not needed, so it is NULL.
/// Is the executor's hook installed -- will an executor-answered call be
/// answered, rather than read as NULL?
pub(crate) fn executor_hook_installed() -> bool {
    !SUPPRESSED.with(|s| s.get()) && SEQUENCE_HOOK.with(|r| r.get()).is_some()
}

pub(crate) fn call_sequence(name: &str, args: &[Bson]) -> Result<Bson> {
    if SUPPRESSED.with(|s| s.get()) {
        return Ok(Bson::Null);
    }
    // A `regclass` argument (`nextval('s'::regclass)`) names the sequence
    // by oid; the hook takes its name.
    let args: Vec<Bson> = args
        .iter()
        .map(|a| match crate::regclass_oid(a) {
            Some(oid) => Bson::String(crate::regclass_text(oid)),
            None => a.clone(),
        })
        .collect();
    match SEQUENCE_HOOK.with(|r| r.get()) {
        // SAFETY: set only inside `with_sequence_hook`, whose borrow is live.
        Some(hook) => unsafe { (*hook)(name, &args) },
        None => Ok(Bson::Null),
    }
}

thread_local! {
    /// Set while an expression is evaluated only to learn its TYPE (a row
    /// expression's sample row): nothing may be drawn or run then.
    static SUPPRESSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with sequence draws and correlated subqueries switched off.
pub(crate) fn without_side_effects<R>(f: impl FnOnce() -> R) -> R {
    let previous = SUPPRESSED.with(|s| s.replace(true));
    let out = f();
    SUPPRESSED.with(|s| s.set(previous));
    out
}

/// A user-defined function (`LANGUAGE sql` / `plpgsql`) as the planner knows
/// it: enough to type a call and to route one.
#[derive(Debug, Clone, PartialEq)]
pub struct UserFn {
    pub name: String,
    pub arg_types: Vec<String>,
    /// The return type; for a set-returning function, of one row's column.
    pub return_type: String,
    pub returns_set: bool,
    /// `RETURNS TABLE (...)` / OUT columns, `(name, type)`.
    pub columns: Vec<(String, String)>,
    /// The last parameter is `VARIADIC`: trailing arguments are packed into
    /// its array.
    pub variadic: bool,
    /// The catalog key the executor finds the function's body under.
    pub key: String,
    /// `STRICT`: any NULL argument answers NULL (no rows, for a set-returning
    /// function) without the body running.
    pub strict: bool,
    /// Each input parameter's `DEFAULT` as SQL, `None` where it has none.
    pub defaults: Vec<Option<String>>,
}

/// What a user function call produced.
pub enum FnResult {
    Value(Bson),
    /// `(column names, column types, rows)`.
    Rows(Vec<String>, Vec<String>, Vec<Vec<Bson>>),
}

/// Runs a user-defined function. Supplied by the executor.
pub type FunctionHook<'a> = dyn Fn(&UserFn, &[Bson]) -> Result<FnResult> + 'a;
type FHook = FunctionHook<'static>;

thread_local! {
    static FUNCTION_HOOK: std::cell::Cell<Option<*const FHook>> = const { std::cell::Cell::new(None) };
    static USER_FUNCTIONS: std::cell::RefCell<Vec<UserFn>> = const { std::cell::RefCell::new(Vec::new()) };
    static USER_PROCEDURES: std::cell::RefCell<Vec<(String, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the user-defined PROCEDURES, `(name, number of inputs)`: a
/// procedure is not a function, so a call of one in an expression is
/// PostgreSQL's 42809.
pub fn set_user_procedures(procs: Vec<(String, usize)>) {
    USER_PROCEDURES.with(|p| *p.borrow_mut() = procs);
}

/// Whether a procedure of this name takes `nargs` inputs -- what an
/// expression calling it resolves to, before 42809 refuses it.
pub fn is_user_procedure(name: &str, nargs: usize) -> bool {
    USER_PROCEDURES.with(|p| p.borrow().iter().any(|(n, k)| n == name && *k == nargs))
}

/// Install the user-defined functions for the statements that follow.
pub fn set_user_functions(fns: Vec<UserFn>) {
    USER_FUNCTIONS.with(|f| *f.borrow_mut() = fns);
}

/// The installed user functions.
pub(crate) fn user_functions() -> Vec<UserFn> {
    USER_FUNCTIONS.with(|f| f.borrow().clone())
}

/// The user function a call resolves to: by name and argument count, and --
/// when overloads share the count -- by the arguments' types, an untyped
/// literal matching any. Several equally good candidates are PostgreSQL's
/// `42725 function ... is not unique`, surfaced by the caller as no match.
pub(crate) fn user_function_for(name: &str, args: &[pg_query::protobuf::Node]) -> Option<UserFn> {
    let candidates: Vec<UserFn> = USER_FUNCTIONS.with(|f| {
        f.borrow()
            .iter()
            .filter(|u| {
                u.name == name
                    && (u.arg_types.len() == args.len()
                        || (u.variadic && args.len() >= u.arg_types.len())
                        // Trailing parameters with a DEFAULT may be left out.
                        || (args.len() < u.arg_types.len()
                            && (args.len()..u.arg_types.len())
                                .all(|i| u.defaults.get(i).is_some_and(Option::is_some))))
            })
            .cloned()
            .collect()
    });
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    let canon = |t: &str| {
        crate::pgtypes::oid_of_name(t)
            .map(|o| o.to_string())
            .unwrap_or_else(|| t.to_ascii_lowercase())
    };
    let untyped = |n: &pg_query::protobuf::Node| {
        matches!(
            n.node.as_ref(),
            Some(pg_query::protobuf::node::Node::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
        )
    };
    let arg_types: Vec<Option<String>> = args
        .iter()
        .map(|a| (!untyped(a)).then(|| canon(&crate::static_type(a, &Bson::Null))))
        .collect();
    // Score: exact type matches; an untyped literal prefers a string type.
    let score = |u: &UserFn| -> Option<i32> {
        let mut s = 0;
        for (i, t) in arg_types.iter().enumerate() {
            let want = canon(u.arg_types.get(i).or(u.arg_types.last())?);
            match t {
                Some(t) if *t == want => s += 2,
                Some(_) => return None,
                None if want == canon("text") => s += 1,
                None => {}
            }
        }
        Some(s)
    };
    let mut best: Vec<(i32, UserFn)> = candidates
        .into_iter()
        .filter_map(|u| score(&u).map(|s| (s, u)))
        .collect();
    best.sort_by_key(|b| std::cmp::Reverse(b.0));
    match best.as_slice() {
        [(s1, _), (s2, _), ..] if s1 == s2 => None,
        [(_, u), ..] => Some(u.clone()),
        [] => None,
    }
}

/// Run `f` able to call user-defined functions.
pub fn with_function_hook<R>(hook: &FunctionHook<'_>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const FHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FUNCTION_HOOK.with(|r| r.set(self.0));
        }
    }
    // SAFETY: as `with_correlated_runner`.
    let ptr: *const FHook =
        unsafe { std::mem::transmute::<*const FunctionHook<'_>, *const FHook>(hook) };
    let _restore = Restore(FUNCTION_HOOK.with(|r| r.replace(Some(ptr))));
    f()
}

/// What a user function call's arguments are, as PostgreSQL's planner sees
/// them -- which decides whether an inlinable `LANGUAGE sql` function is
/// folded while planning (an error then carries `SQL function "f" during
/// inlining`) or runs with the statement (no frame at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallArgs {
    /// Something only known per row or per call: a column, a volatile or
    /// built-in function, a subquery. Not folded.
    Runtime,
    /// Constants only (and calls of user functions over constants).
    Constant,
    /// Constants and the statement's bound parameters, which a custom plan
    /// folds like constants -- unless the statement runs inside a function
    /// body, where they are that body's variables.
    WithParams,
}

thread_local! {
    static CALL_ARGS: std::cell::Cell<CallArgs> = const { std::cell::Cell::new(CallArgs::Runtime) };
    static ROW_PARAMS_FROM: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// The kind of the user function call now running (`Runtime` outside one).
pub fn current_call_args() -> CallArgs {
    CALL_ARGS.with(|c| c.get())
}

pub(crate) fn with_call_args_kind<R>(kind: CallArgs, f: impl FnOnce() -> R) -> R {
    let previous = CALL_ARGS.with(|c| c.replace(kind));
    let out = f();
    CALL_ARGS.with(|c| c.set(previous));
    out
}

/// Evaluate `f` with parameters numbered past `n` being a row's columns.
pub(crate) fn with_row_params_from<R>(n: usize, f: impl FnOnce() -> R) -> R {
    let previous = ROW_PARAMS_FROM.with(|c| c.replace(n));
    let out = f();
    ROW_PARAMS_FROM.with(|c| c.set(previous));
    out
}

/// Is every PostgreSQL 15 built-in `name` taking `nargs` arguments
/// IMMUTABLE (`pg15_immutable_functions.tsv`, dumped from `pg_proc`)?
pub(crate) fn immutable_builtin(name: &str, nargs: usize) -> bool {
    static SET: std::sync::OnceLock<std::collections::HashSet<(&'static str, usize)>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| {
        include_str!("pg15_immutable_functions.tsv")
            .lines()
            .filter_map(|l| {
                let (n, a) = l.split_once('\t')?;
                Some((n, a.trim().parse().ok()?))
            })
            .collect()
    })
    .contains(&(name, nargs))
}

/// Classify a call's arguments (see [`CallArgs`]).
pub(crate) fn call_args_kind(args: &[pg_query::protobuf::Node]) -> CallArgs {
    let row_from = ROW_PARAMS_FROM.with(|c| c.get());
    let mut kind = CallArgs::Constant;
    for a in args {
        let mut node = a.clone();
        let _ = crate::walk_expr(&mut node, &mut |n| {
            match n.node.as_ref() {
                Some(N::ColumnRef(_)) | Some(N::SubLink(_)) => kind = CallArgs::Runtime,
                Some(N::ParamRef(p)) => {
                    if p.number as usize > row_from {
                        kind = CallArgs::Runtime;
                    } else if kind == CallArgs::Constant {
                        kind = CallArgs::WithParams;
                    }
                }
                Some(N::FuncCall(f)) => {
                    let user = func_name(f).is_some_and(|name| {
                        user_function_for(&name, &f.args).is_some_and(|u| !u.returns_set)
                    });
                    // An IMMUTABLE built-in is folded over constants while
                    // planning (`f(abs(0))` is inlined and folded); a stable
                    // or volatile one, or an aggregate / window call, runs.
                    let folded = f.over.is_none()
                        && !f.agg_star
                        && f.agg_order.is_empty()
                        && func_name(f).is_some_and(|name| {
                            !user_function_named(&name) && immutable_builtin(&name, f.args.len())
                        });
                    if !user && !folded {
                        kind = CallArgs::Runtime;
                    }
                }
                _ => {}
            }
            Ok(())
        });
        if kind == CallArgs::Runtime {
            break;
        }
    }
    kind
}

/// Call a user function. With no hook installed (a Describe), or while only
/// a TYPE is wanted, nothing runs and the answer is NULL.
pub(crate) fn call_user_function(u: &UserFn, args: &[Bson]) -> Result<FnResult> {
    if SUPPRESSED.with(|s| s.get()) {
        return Ok(FnResult::Value(Bson::Null));
    }
    // Parameters the call left out take their DEFAULT, evaluated now and
    // cast to the parameter's type.
    let defaulted;
    let args = if args.len() < u.arg_types.len() && !u.variadic {
        let mut out = args.to_vec();
        for i in args.len()..u.arg_types.len() {
            let sql = u.defaults.get(i).cloned().flatten().ok_or_else(|| {
                Error::UndefinedFunction(format!("function {}() does not exist", u.name))
            })?;
            let node = crate::domains::parse_default_sql(&sql)?;
            let v = crate::const_value(&node, &[])?;
            out.push(crate::cast_value(v, &u.arg_types[i])?);
        }
        defaulted = out;
        &defaulted[..]
    } else {
        args
    };
    if u.strict && args.contains(&Bson::Null) {
        return Ok(if u.returns_set {
            FnResult::Rows(Vec::new(), Vec::new(), Vec::new())
        } else {
            FnResult::Value(Bson::Null)
        });
    }
    // A VARIADIC call packs its trailing arguments into the last parameter's
    // array, unless it passed that array itself (`VARIADIC ARRAY[...]`).
    let packed;
    let args = if u.variadic && !u.arg_types.is_empty() {
        let fixed = u.arg_types.len() - 1;
        let explicit = args.len() == u.arg_types.len() && matches!(args[fixed], Bson::Array(_));
        if explicit {
            args
        } else {
            let mut out = args[..fixed.min(args.len())].to_vec();
            out.push(Bson::Array(args[fixed.min(args.len())..].to_vec()));
            packed = out;
            &packed[..]
        }
    } else {
        args
    };
    match FUNCTION_HOOK.with(|r| r.get()) {
        // SAFETY: set only inside `with_function_hook`, whose borrow is live.
        Some(hook) => unsafe { (*hook)(u, args) },
        None => Ok(if u.returns_set {
            FnResult::Rows(Vec::new(), Vec::new(), Vec::new())
        } else {
            FnResult::Value(Bson::Null)
        }),
    }
}
