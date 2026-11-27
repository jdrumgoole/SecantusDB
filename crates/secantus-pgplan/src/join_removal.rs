//! `remove_useless_joins` for an EXPLAINed SELECT (RP-1060): a LEFT JOIN
//! whose inner side is one plain table, read nowhere but in the join's own
//! ON, and unique on the columns that ON equates to the outer side or to a
//! constant, is removed, and the query is planned as its outer side alone
//! (`join_is_removable`). Each outer row then matches at most one inner
//! row, and nothing reads that row, so the join only ever returns its
//! outer rows once each.
//!
//! The planner does not know the unique indexes, so it plans the removed
//! form beside the written one, as `reduce_unique_semijoins` is (RP-411):
//! each [`JoinRemoval`] names the inner table and the columns its ON fixes,
//! and the server takes the removed plan when a unique index of that table
//! -- immediate, without a predicate, on plain columns -- has every key
//! fixed (`relation_has_unique_index_for`).
//!
//! The removed relation stays in PostgreSQL's range table, so EXPLAIN
//! still prefixes the column names it would prefix in a plan of two
//! relations (`useprefix`): [`joins_removed`] tells the planning of the
//! removed form so.
//!
//! Recognised here: an ON equality whose inner operand is a bare column and
//! whose other operand is a column of the outer side, a literal or a cast
//! of either, of the inner column's type or one its B-tree operator family
//! compares across (the integers, the floats, `text` and `varchar`), and a
//! USING column. An equality of any other shape does not fix its column
//! here, which only ever leaves the join in place.
//!
//! The inner side is taken as `pull_up_subqueries` leaves it (RP-1106): a
//! view, a CTE `inline_cte` inlines and a FROM subquery are each pulled up
//! when `is_simple_subquery` holds, so a chain of them over one plain
//! table is that table, each column it shows mapped to the table's column
//! it is, and the WHERE it carries a restriction of that table (an equality
//! to a constant there fixes its column, as a `baserestrictinfo` does for
//! `relation_has_unique_index_for`). One with no FROM and no WHERE pulls
//! up to an `RTE_RESULT`, a single row, and a LEFT JOIN to that is removed
//! whatever its ON says (`remove_useless_result_rtes`), before any unique
//! index is asked.

use super::*;
use pg_query::protobuf::{ColumnRef, CommonTableExpr, JoinExpr, JoinType, Node, SelectStmt};

/// A LEFT JOIN the server removes when its inner table is unique on
/// `fixed`.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRemoval {
    /// The inner table, as the catalog names it.
    pub table: String,
    /// The inner table's columns the join's ON fixes: each equated to a
    /// column of the outer side or to a constant.
    pub fixed: Vec<String>,
    /// The EXPLAIN of the query with the join replaced by its outer side.
    pub removed: Box<Statement>,
}

thread_local! {
    static REMOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the EXPLAIN being planned is one with a join removed: its range
/// table holds more relations than its FROM shows.
pub(crate) fn joins_removed() -> bool {
    REMOVED.with(std::cell::Cell::get)
}

/// `f`, planned as a query with a join removed.
pub(crate) fn with_joins_removed<R>(f: impl FnOnce() -> R) -> R {
    let was = REMOVED.with(|r| r.replace(true));
    let out = f();
    REMOVED.with(|r| r.set(was));
    out
}

/// A removable LEFT JOIN of `query`: the inner table, the columns its ON
/// fixes, and the query without it.
pub(crate) struct Candidate {
    pub(crate) table: String,
    pub(crate) fixed: Vec<String>,
    pub(crate) query: N,
    /// Removed with no index asked: its inner side is one row
    /// (`remove_useless_result_rtes`; `table` is empty).
    pub(crate) always: bool,
}

/// Every LEFT JOIN of `query`'s FROM that is removable if its inner table
/// is unique on the columns its ON fixes, in the order the FROM writes
/// them.
pub(crate) fn candidates(query: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Vec<Candidate> {
    let N::SelectStmt(s) = query else {
        return Vec::new();
    };
    // A row mark reads every relation's row identity above the join.
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || !s.locking_clause.is_empty()
        || s.into_clause.is_some()
        || !s.values_lists.is_empty()
    {
        return Vec::new();
    }
    let ctes = statement_ctes(s);
    let mut joins = Vec::new();
    for item in &s.from_clause {
        collect_joins(item, &mut joins);
    }
    let mut out = Vec::new();
    for (i, j) in joins.iter().enumerate() {
        let Some((table, fixed)) = removable(s, &joins, i, j, &ctes, lookup) else {
            continue;
        };
        let always = table.is_none();
        let table = table.unwrap_or_default();
        let mut rewritten: SelectStmt = (**s).clone();
        let mut seen = 0;
        for item in &mut rewritten.from_clause {
            if replace_join(item, i, &mut seen) {
                break;
            }
        }
        // A CTE read only there is read nowhere now (`inline_cte` leaves
        // no CTE behind).
        if let Some(N::RangeVar(rv)) = j.rarg.as_deref().and_then(|r| r.node.as_ref()) {
            if ctes
                .iter()
                .any(|(c, refs)| c.ctename == rv.relname && *refs == 1)
            {
                if let Some(with) = rewritten.with_clause.as_mut() {
                    with.ctes.retain(|c| {
                        !matches!(c.node.as_ref(),
                            Some(N::CommonTableExpr(c)) if c.ctename == rv.relname)
                    });
                    if with.ctes.is_empty() {
                        rewritten.with_clause = None;
                    }
                }
            }
        }
        out.push(Candidate {
            table,
            fixed,
            query: N::SelectStmt(Box::new(rewritten)),
            always,
        });
    }
    out
}

/// The FROM item's joins, each before its sides, left before right.
fn collect_joins<'a>(n: &'a Node, out: &mut Vec<&'a JoinExpr>) {
    if let Some(N::JoinExpr(j)) = n.node.as_ref() {
        out.push(j);
        for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
            collect_joins(side, out);
        }
    }
}

/// The `target`-th join (in [`collect_joins`]'s order) replaced by its
/// outer side.
fn replace_join(n: &mut Node, target: usize, seen: &mut usize) -> bool {
    let Some(N::JoinExpr(j)) = n.node.as_mut() else {
        return false;
    };
    if *seen == target {
        if let Some(larg) = j.larg.take() {
            *n = *larg;
        }
        return true;
    }
    *seen += 1;
    for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        if replace_join(side, target, seen) {
            return true;
        }
    }
    false
}

/// The `target`-th join's ON and inner side dropped, so that what is left
/// reads only what the query reads outside them: a subquery's own column
/// references are its own.
fn clear_quals(n: &mut Node, target: usize, seen: &mut usize) -> bool {
    let Some(N::JoinExpr(j)) = n.node.as_mut() else {
        return false;
    };
    if *seen == target {
        j.quals = None;
        j.rarg = None;
        return true;
    }
    *seen += 1;
    for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        if clear_quals(side, target, seen) {
            return true;
        }
    }
    false
}

/// A relation of a join's outer side: its name and its table.
struct Rel {
    refname: String,
    def: TableDef,
}

/// The plain relations of a join's outer side, or `None` when it holds
/// anything else (a subquery, a function, a CTE), whose columns are not
/// known here.
fn outer_rels(
    n: &Node,
    ctes: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<Rel>,
) -> Option<()> {
    match n.node.as_ref()? {
        N::RangeVar(rv) if rv.schemaname.is_empty() && !ctes.contains(&rv.relname) => {
            if rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
                return None;
            }
            let def = lookup(&rv.relname)?;
            out.push(Rel {
                refname: shown_name(rv),
                def,
            });
            Some(())
        }
        N::JoinExpr(j) => {
            outer_rels(j.larg.as_deref()?, ctes, lookup, out)?;
            outer_rels(j.rarg.as_deref()?, ctes, lookup, out)
        }
        _ => None,
    }
}

fn shown_name(rv: &pg_query::protobuf::RangeVar) -> String {
    rv.alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_else(|| rv.relname.clone())
}

fn string_names(list: &[Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// The statement's CTEs, each with how many references to its name the
/// statement holds (`cterefcount`): any relation of that name, anywhere in
/// it, so an over-count only keeps a CTE whole.
fn statement_ctes(s: &SelectStmt) -> Vec<(&CommonTableExpr, usize)> {
    let Some(with) = s.with_clause.as_ref() else {
        return Vec::new();
    };
    let whole = N::SelectStmt(Box::new(s.clone()));
    let names: Vec<String> = whole
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::RangeVar(rv) if rv.schemaname.is_empty() => Some(rv.relname.clone()),
            _ => None,
        })
        .collect();
    with.ctes
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(&**c),
            _ => None,
        })
        .map(|c| (c, names.iter().filter(|n| **n == c.ctename).count()))
        .collect()
}

/// What a FROM item names, read where a LEFT JOIN's inner side is: the
/// statement's CTEs, or (inside a subquery or a view) none of them.
struct Scope<'a> {
    /// The statement's CTEs and their reference counts; `recursive` for
    /// `WITH RECURSIVE`, which inlines none.
    ctes: &'a [(&'a CommonTableExpr, usize)],
    recursive: bool,
    /// Inside a subquery a reference to one of the statement's CTEs is
    /// left alone; inside a view none is visible.
    nested: bool,
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
}

/// A LEFT JOIN's inner side as `pull_up_subqueries` leaves it.
enum Pulled {
    /// One plain table: each column the inner side shows, by the name the
    /// query reads it under, with the table's column it is (`None` for an
    /// expression), and the table's columns a pulled-up WHERE equates to
    /// a constant.
    Table {
        def: Box<TableDef>,
        shown: Vec<(String, Option<String>)>,
        restricted: Vec<String>,
    },
    /// No FROM and no WHERE: one row, an `RTE_RESULT` once pulled up; the
    /// names of the columns it shows.
    OneRow(Vec<String>),
}

impl Pulled {
    fn names(&self) -> Vec<String> {
        match self {
            Pulled::Table { shown, .. } => shown.iter().map(|(n, _)| n.clone()).collect(),
            Pulled::OneRow(names) => names.clone(),
        }
    }

    /// The columns it shows as a relation `refname`, each typed as the
    /// table's column it is (`unknown` for any other).
    fn relation(&self, refname: &str) -> TableDef {
        let columns = match self {
            Pulled::Table { def, shown, .. } => shown
                .iter()
                .map(|(n, base)| {
                    let t = base
                        .as_deref()
                        .and_then(|b| column(def, b))
                        .map_or("unknown", |c| c.pg_type.as_str());
                    secantus_pgcatalog::Column::new(n, t, false)
                })
                .collect(),
            Pulled::OneRow(names) => names
                .iter()
                .map(|n| secantus_pgcatalog::Column::new(n, "unknown", false))
                .collect(),
        };
        TableDef::new(refname, columns)
    }

    /// Renamed by a column alias list, as `AS s(x, y)` renames the first
    /// columns; `None` for more names than columns.
    fn renamed(mut self, names: &[String]) -> Option<Self> {
        if names.is_empty() {
            return Some(self);
        }
        if names.len() > self.names().len() {
            return None;
        }
        match &mut self {
            Pulled::Table { shown, .. } => {
                for (col, name) in shown.iter_mut().zip(names) {
                    col.0 = name.clone();
                }
            }
            Pulled::OneRow(cols) => {
                for (col, name) in cols.iter_mut().zip(names) {
                    *col = name.clone();
                }
            }
        }
        Some(self)
    }
}

/// The FROM item `n` as `pull_up_subqueries` leaves it -- a plain table, or
/// a view, an inlined CTE or a FROM subquery simple enough to pull up --
/// and the name it is read under. `None` for anything else.
fn pulled_item(n: &Node, scope: &Scope<'_>, depth: usize) -> Option<(String, Pulled)> {
    if depth > 16 {
        return None;
    }
    match n.node.as_ref()? {
        N::RangeVar(rv) => {
            if !rv.schemaname.is_empty() {
                return None;
            }
            let colnames = rv
                .alias
                .as_ref()
                .map(|a| string_names(&a.colnames))
                .unwrap_or_default();
            let cte = scope
                .ctes
                .iter()
                .find(|(c, _)| c.ctename == rv.relname)
                .copied();
            let pulled = if let Some((cte, refs)) = cte {
                if scope.nested {
                    return None;
                }
                let body = inlined_cte(cte, refs, scope.recursive)?;
                let inner = Scope {
                    nested: true,
                    ..*scope
                };
                pulled_select(body, &inner, depth + 1)?
                    .renamed(&string_names(&cte.aliascolnames))?
            } else if crate::is_view(&rv.relname) {
                // A security barrier view is not pulled up
                // (`is_simple_subquery`).
                if crate::is_security_barrier_view(&rv.relname) {
                    return None;
                }
                let N::SelectStmt(body) = parse_one(&crate::view_definition(&rv.relname)?).ok()?
                else {
                    return None;
                };
                let inner = Scope {
                    ctes: &[],
                    recursive: false,
                    nested: true,
                    lookup: scope.lookup,
                };
                pulled_select(&body, &inner, depth + 1)?
            } else {
                if !colnames.is_empty() {
                    return None;
                }
                // A parent with children reads them too, which its own
                // indexes do not cover (`get_relation_info` gives an
                // inheritance parent none).
                if rv.inh && !crate::inherit::descendants(&rv.relname).is_empty() {
                    return None;
                }
                let def = (scope.lookup)(&rv.relname)?;
                let shown = def
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), Some(c.name.clone())))
                    .collect();
                Pulled::Table {
                    def: Box::new(def),
                    shown,
                    restricted: Vec::new(),
                }
            };
            Some((shown_name(rv), pulled.renamed(&colnames)?))
        }
        N::RangeSubselect(rs) if !rs.lateral => {
            let alias = rs.alias.as_ref()?;
            let Some(N::SelectStmt(body)) = rs.subquery.as_deref()?.node.as_ref() else {
                return None;
            };
            let inner = Scope {
                nested: true,
                ..*scope
            };
            let pulled = pulled_select(body, &inner, depth + 1)?;
            Some((
                alias.aliasname.clone(),
                pulled.renamed(&string_names(&alias.colnames))?,
            ))
        }
        _ => None,
    }
}

/// The body of a CTE `inline_cte` inlines: not recursive, not
/// MATERIALIZED, read once (or NOT MATERIALIZED), a SELECT with no
/// volatile call anywhere in it.
fn inlined_cte(cte: &CommonTableExpr, refs: usize, recursive: bool) -> Option<&SelectStmt> {
    use pg_query::protobuf::CteMaterialize;
    let m = cte.ctematerialized;
    if recursive
        || cte.cterecursive
        || m == CteMaterialize::Always as i32
        || (m != CteMaterialize::Never as i32 && refs != 1)
    {
        return None;
    }
    let Some(N::SelectStmt(body)) = cte.ctequery.as_deref()?.node.as_ref() else {
        return None;
    };
    let whole = N::SelectStmt(body.clone());
    let volatile = whole.nodes().into_iter().any(|(n, _, _, _)| match n {
        pg_query::NodeRef::FuncCall(f) => volatile_call(f),
        _ => false,
    });
    (!volatile).then_some(&**body)
}

/// A call `contain_volatile_functions` finds: a volatile built-in, or a
/// user function (whose volatility is not known here).
fn volatile_call(f: &pg_query::protobuf::FuncCall) -> bool {
    crate::func_name(f).is_none_or(|name| {
        crate::correlated::volatile_builtin(&name, f.args.len())
            || crate::correlated::user_function_named(&name)
    })
}

/// The subquery `sub` pulled up (`is_simple_subquery`, then
/// `pull_up_simple_subquery`): a plain SELECT of one FROM item that pulls
/// up in turn, or of none, whose select list has no aggregate, window
/// function, set-returning or volatile call. `None` when it stays a
/// subquery.
fn pulled_select(sub: &SelectStmt, scope: &Scope<'_>, depth: usize) -> Option<Pulled> {
    let plain = sub.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && sub.with_clause.is_none()
        && sub.group_clause.is_empty()
        && sub.having_clause.is_none()
        && sub.distinct_clause.is_empty()
        && sub.limit_count.is_none()
        && sub.limit_offset.is_none()
        && sub.sort_clause.is_empty()
        && sub.window_clause.is_empty()
        && sub.locking_clause.is_empty()
        && sub.into_clause.is_none()
        && sub.values_lists.is_empty();
    if !plain {
        return None;
    }
    let mut targets = Vec::new();
    for t in &sub.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        let val = rt.val.as_deref()?;
        // A sublink in the select list is left to the planner: its own
        // aggregates are not this level's.
        let refused = val
            .node
            .as_ref()?
            .nodes()
            .into_iter()
            .any(|(n, _, _, _)| match n {
                pg_query::NodeRef::SubLink(_) => true,
                pg_query::NodeRef::FuncCall(f) => {
                    f.over.is_some()
                        || crate::is_aggregate_call(f)
                        || crate::srf_place::is_srf(f)
                        || volatile_call(f)
                }
                _ => false,
            });
        if refused {
            return None;
        }
        targets.push((rt.name.clone(), val));
    }
    match sub.from_clause.as_slice() {
        [] => {
            if sub.where_clause.is_some() {
                return None;
            }
            let mut names = Vec::new();
            for (name, val) in targets {
                if is_star(val) {
                    return None;
                }
                names.push(output_name(&name, val));
            }
            Some(Pulled::OneRow(names))
        }
        [item] => {
            let (refname, inner) = pulled_item(item, scope, depth)?;
            let inner_names = inner.names();
            // Each output: the inner column it reads, by position, when it
            // reads one bare.
            let mut outputs: Vec<(String, Option<usize>)> = Vec::new();
            for (name, val) in targets {
                if let Some(N::ColumnRef(c)) = val.node.as_ref() {
                    let p = parts(c)?;
                    let col = match p.as_slice() {
                        [None] => None,
                        [Some(q), None] if *q == refname => None,
                        [Some(col)] => Some(*col),
                        [Some(q), Some(col)] if *q == refname => Some(*col),
                        _ => return None,
                    };
                    match col {
                        // `*`: every inner column, in order.
                        None => outputs.extend(
                            inner_names
                                .iter()
                                .cloned()
                                .zip(0..)
                                .map(|(n, i)| (n, Some(i))),
                        ),
                        Some(col) => {
                            let i = inner_names.iter().position(|n| n == col)?;
                            outputs.push((output_name(&name, val), Some(i)));
                        }
                    }
                    continue;
                }
                outputs.push((output_name(&name, val), None));
            }
            match inner {
                Pulled::OneRow(_) => {
                    if sub.where_clause.is_some() {
                        return None;
                    }
                    Some(Pulled::OneRow(
                        outputs.into_iter().map(|(n, _)| n).collect(),
                    ))
                }
                Pulled::Table {
                    def,
                    shown,
                    mut restricted,
                } => {
                    if let Some(w) = sub.where_clause.as_deref() {
                        let mut conjuncts = Vec::new();
                        and_conjuncts(w, &mut conjuncts);
                        for c in conjuncts {
                            let Some(base) = restricted_column(c, &refname, &shown, &def) else {
                                continue;
                            };
                            if !restricted.contains(&base) {
                                restricted.push(base);
                            }
                        }
                    }
                    let shown_out = outputs
                        .into_iter()
                        .map(|(n, i)| (n, i.and_then(|i| shown[i].1.clone())))
                        .collect();
                    Some(Pulled::Table {
                        def,
                        shown: shown_out,
                        restricted,
                    })
                }
            }
        }
        _ => None,
    }
}

fn is_star(n: &Node) -> bool {
    matches!(n.node.as_ref(), Some(N::ColumnRef(c))
        if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_)))))
}

/// The name a select-list item shows: its alias, else the name
/// `FigureColname` gives it.
fn output_name(alias: &str, val: &Node) -> String {
    if !alias.is_empty() {
        return alias.to_string();
    }
    fn figure(n: &Node) -> Option<String> {
        match n.node.as_ref()? {
            N::ColumnRef(c) => match c.fields.last()?.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            },
            N::FuncCall(f) => match f.funcname.last()?.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            },
            N::TypeCast(tc) => figure(tc.arg.as_deref()?).or_else(|| {
                tc.type_name
                    .as_ref()
                    .and_then(|t| t.names.last())
                    .and_then(|n| match n.node.as_ref()? {
                        N::String(s) => Some(s.sval.clone()),
                        _ => None,
                    })
            }),
            N::CaseExpr(_) => Some("case".into()),
            _ => None,
        }
    }
    figure(val).unwrap_or_else(|| "?column?".into())
}

/// The table column a pulled-up WHERE conjunct `col = constant` fixes:
/// `col` one of the inner relation's columns (`refname`, `shown`) that is
/// a column of the table, the constant of its type or of one its B-tree
/// operator family compares across.
fn restricted_column(
    c: &Node,
    refname: &str,
    shown: &[(String, Option<String>)],
    def: &TableDef,
) -> Option<String> {
    let N::AExpr(e) = c.node.as_ref()? else {
        return None;
    };
    if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !is_equals(&e.name) {
        return None;
    }
    let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
    for (col, other) in [(l, r), (r, l)] {
        let Some(N::ColumnRef(cr)) = col.node.as_ref() else {
            continue;
        };
        let name = match parts(cr)?.as_slice() {
            [Some(n)] => *n,
            [Some(q), Some(n)] if *q == refname => *n,
            _ => continue,
        };
        let base = shown.iter().find(|(n, _)| n == name)?.1.clone()?;
        let t = constant_type(other)?;
        return compatible(&t, &column(def, &base)?.pg_type).then_some(base);
    }
    None
}

/// The type of a constant: a literal (`unknown` for a quoted one), or a
/// cast of one.
fn constant_type(n: &Node) -> Option<String> {
    match n.node.as_ref()? {
        N::AConst(c) => literal_type(c),
        N::TypeCast(tc) if matches!(tc.arg.as_deref()?.node.as_ref(), Some(N::AConst(_))) => {
            constant_type(tc.arg.as_deref()?)?;
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.ends_with("[]")).then_some(t)
        }
        _ => None,
    }
}

fn literal_type(c: &pg_query::protobuf::AConst) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    if c.isnull {
        return None;
    }
    match c.val.as_ref()? {
        Val::Ival(_) => Some("int4".into()),
        Val::Fval(f) if f.fval.bytes().all(|b| b.is_ascii_digit()) => Some(
            if f.fval.parse::<i64>().is_ok() {
                "int8"
            } else {
                "numeric"
            }
            .into(),
        ),
        Val::Fval(_) => Some("numeric".into()),
        Val::Sval(_) => Some("unknown".into()),
        _ => None,
    }
}

/// Whether `j`, the `index`-th of `joins`, is a LEFT JOIN that is
/// removable: `Some((None, _))` whatever any index says (its inner side is
/// one row), `Some((Some(table), fixed))` if `table` is unique on `fixed`.
fn removable(
    s: &SelectStmt,
    joins: &[&JoinExpr],
    index: usize,
    j: &JoinExpr,
    ctes: &[(&CommonTableExpr, usize)],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<(Option<String>, Vec<String>)> {
    if j.jointype != JoinType::JoinLeft as i32
        || j.is_natural
        || j.alias.is_some()
        || j.join_using_alias.is_some()
    {
        return None;
    }
    let scope = Scope {
        ctes,
        recursive: s.with_clause.as_ref().is_some_and(|w| w.recursive),
        nested: false,
        lookup,
    };
    let (refname, pulled) = pulled_item(j.rarg.as_deref()?, &scope, 0)?;
    let def = pulled.relation(&refname);
    let cte_names: Vec<String> = ctes.iter().map(|(c, _)| c.ctename.clone()).collect();
    let using = string_names(&j.using_clause);
    // Another join's USING or NATURAL can read the inner table's columns
    // by name.
    for (i, other) in joins.iter().enumerate() {
        if i == index {
            continue;
        }
        if other.is_natural
            || string_names(&other.using_clause)
                .iter()
                .any(|u| has_column(&def, u) && !using.contains(u))
        {
            return None;
        }
    }
    // Nothing outside the join's ON may read the inner relation
    // (`attr_needed` within the join). The CTE it reads is its own scope.
    let mut probe = s.clone();
    let mut seen = 0;
    for item in &mut probe.from_clause {
        if clear_quals(item, index, &mut seen) {
            break;
        }
    }
    if let (Some(with), Some(N::RangeVar(rv))) = (
        probe.with_clause.as_mut(),
        j.rarg.as_deref().and_then(|r| r.node.as_ref()),
    ) {
        with.ctes.retain(
            |c| !matches!(c.node.as_ref(), Some(N::CommonTableExpr(c)) if c.ctename == rv.relname),
        );
    }
    let probe = N::SelectStmt(Box::new(probe));
    if probe.nodes().into_iter().any(|(r, _, _, _)| match r {
        pg_query::NodeRef::ColumnRef(c) => may_read(c, &refname, &def, &using),
        _ => false,
    }) {
        return None;
    }
    let (base, shown, restricted) = match pulled {
        // `remove_useless_result_rtes`: nothing reads a row that is always
        // there, so the join returns each outer row once.
        Pulled::OneRow(_) => return Some((None, Vec::new())),
        Pulled::Table {
            def,
            shown,
            restricted,
        } => (def, shown, restricted),
    };
    // The table's column an inner column the query reads is, and its type.
    let base_column = |inner: &str| -> Option<&Column> {
        let b = shown.iter().find(|(n, _)| n == inner)?.1.as_deref()?;
        column(&base, b)
    };
    let mut outer = Vec::new();
    let outer_complete = outer_rels(j.larg.as_deref()?, &cte_names, lookup, &mut outer).is_some();
    let mut fixed: Vec<String> = restricted;
    for u in &using {
        let Some(inner) = base_column(u) else {
            continue;
        };
        let mut hits = outer.iter().filter_map(|r| column(&r.def, u));
        if let (Some(o), None, true) = (hits.next(), hits.next(), outer_complete) {
            if compatible(&o.pg_type, &inner.pg_type) && !fixed.contains(&inner.name) {
                fixed.push(inner.name.clone());
            }
        }
    }
    let mut conjuncts = Vec::new();
    if let Some(q) = j.quals.as_deref() {
        and_conjuncts(q, &mut conjuncts);
    }
    for c in conjuncts {
        let Some(N::AExpr(e)) = c.node.as_ref() else {
            continue;
        };
        if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !is_equals(&e.name) {
            continue;
        }
        let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
            continue;
        };
        for (inner, other) in [(l, r), (r, l)] {
            let Some(col) = inner_column(inner, &refname, &def, &outer, outer_complete) else {
                continue;
            };
            let side = Side {
                refname: &refname,
                def: &def,
                outer: &outer,
                outer_complete,
                larg: j.larg.as_deref()?,
                lookup,
            };
            if let (Some(t), Some(b)) = (other_type(other, &side), base_column(&col.name)) {
                if compatible(&t, &b.pg_type) && !fixed.contains(&b.name) {
                    fixed.push(b.name.clone());
                }
            }
            break;
        }
    }
    if fixed.is_empty() {
        return None;
    }
    Some((Some(base.name.clone()), fixed))
}

fn column<'a>(def: &'a TableDef, name: &str) -> Option<&'a Column> {
    def.columns.iter().find(|c| c.name == name)
}

fn has_column(def: &TableDef, name: &str) -> bool {
    column(def, name).is_some()
}

/// A column reference's name parts, `None` for a `*`; `None` overall for
/// a part of any other kind.
fn parts(c: &ColumnRef) -> Option<Vec<Option<&str>>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(Some(s.sval.as_str())),
            Some(N::AStar(_)) => Some(None),
            _ => None,
        })
        .collect()
}

/// Whether a column reference may read the inner relation `refname`: a
/// `*`, its name (a whole-row reference), a name it qualifies, or a bare
/// name that is one of its columns -- other than a USING column of the
/// join, which is the outer side's.
fn may_read(c: &ColumnRef, refname: &str, def: &TableDef, using: &[String]) -> bool {
    let Some(parts) = parts(c) else {
        return true;
    };
    match parts.as_slice() {
        [None] => true,
        [Some(n)] => *n == refname || (has_column(def, n) && !using.iter().any(|u| u == n)),
        [.., q, _] => *q == Some(refname),
        [] => true,
    }
}

fn and_conjuncts<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                and_conjuncts(a, out);
            }
        }
        _ => out.push(n),
    }
}

fn is_equals(names: &[Node]) -> bool {
    matches!(
        names,
        [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "=")
    )
}

/// The inner relation's column `n` is, when it is a bare reference to one.
fn inner_column<'a>(
    n: &Node,
    refname: &str,
    def: &'a TableDef,
    outer: &[Rel],
    outer_complete: bool,
) -> Option<&'a Column> {
    let Some(N::ColumnRef(c)) = n.node.as_ref() else {
        return None;
    };
    match parts(c)?.as_slice() {
        [Some(q), Some(col)] if *q == refname => column(def, col),
        [Some(col)] if outer_complete && !outer.iter().any(|r| has_column(&r.def, col)) => {
            column(def, col)
        }
        _ => None,
    }
}

/// What an equality's other operand is typed against: the inner relation
/// it must not read, and the join's outer side.
struct Side<'a> {
    refname: &'a str,
    def: &'a TableDef,
    outer: &'a [Rel],
    outer_complete: bool,
    larg: &'a Node,
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
}

/// The type of an equality's operand over the outer side or constants: a
/// column of the outer side, a literal (`unknown` for a quoted one, which
/// takes the other operand's type), or an expression over those, typed in
/// the outer side's scope. `None` for an operand that may read the inner
/// relation, holds a subquery or a volatile function (not mergejoinable,
/// `check_mergejoinable`), or is not typed here.
fn other_type(n: &Node, side: &Side<'_>) -> Option<String> {
    let (refname, def, outer, outer_complete) =
        (side.refname, side.def, side.outer, side.outer_complete);
    match n.node.as_ref()? {
        N::AConst(c) if !c.isnull => literal_type(c),
        N::TypeCast(tc)
            if matches!(
                tc.arg.as_deref().and_then(|a| a.node.as_ref()),
                Some(N::ColumnRef(_) | N::AConst(_))
            ) =>
        {
            other_type(tc.arg.as_deref()?, side)?;
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.ends_with("[]")).then_some(t)
        }
        N::ColumnRef(c) => {
            if may_read(c, refname, def, &[]) {
                return None;
            }
            match parts(c)?.as_slice() {
                [Some(q), Some(col)] => {
                    let rel = outer.iter().find(|r| r.refname == *q)?;
                    Some(column(&rel.def, col)?.pg_type.clone())
                }
                [Some(col)] if outer_complete => {
                    let mut hits = outer.iter().filter_map(|r| column(&r.def, col));
                    match (hits.next(), hits.next()) {
                        (Some(c), None) => Some(c.pg_type.clone()),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        N::AConst(_) => None,
        e => {
            let readable = e.nodes().into_iter().all(|(r, _, _, _)| match r {
                pg_query::NodeRef::ColumnRef(c) => !may_read(c, refname, def, &[]),
                pg_query::NodeRef::SubLink(_) => false,
                pg_query::NodeRef::FuncCall(f) => crate::func_name(f).is_some_and(|name| {
                    !crate::correlated::volatile_builtin(&name, f.args.len())
                        && !crate::correlated::user_function_named(&name)
                }),
                _ => true,
            });
            if !readable {
                return None;
            }
            let scope = SelectStmt {
                from_clause: vec![side.larg.clone()],
                ..Default::default()
            };
            crate::optype::type_in_select(&scope, n, side.lookup)
        }
    }
}

/// Whether an equality of a `other` operand with an inner column of type
/// `inner` is one of the inner column's B-tree operator family: the same
/// type, an untyped literal, or a type of the same cross-type family.
fn compatible(other: &str, inner: &str) -> bool {
    let family = |t: &str| match t {
        "int2" | "int4" | "int8" => 1,
        "float4" | "float8" => 2,
        "text" | "varchar" => 3,
        _ => 0,
    };
    other == "unknown" || other == inner || (family(other) != 0 && family(other) == family(inner))
}

#[cfg(test)]
mod rp1060_join_removal {
    use super::*;

    /// Each candidate's inner table and fixed columns, and the query
    /// without it.
    fn removals(sql: &str) -> Vec<(String, Vec<String>, String)> {
        let lookup = |name: &str| -> Option<TableDef> {
            let cols: &[(&str, &str)] = match name {
                "e" => &[("a", "int4"), ("b", "text")],
                "k" => &[("a", "int4"), ("b", "text")],
                "h" => &[("a", "int8"), ("b", "text")],
                "n" => &[("a", "numeric"), ("b", "text")],
                _ => return None,
            };
            Some(TableDef::new(
                name,
                cols.iter()
                    .map(|(c, t)| secantus_pgcatalog::Column::new(c, t, false))
                    .collect(),
            ))
        };
        let tree = pg_query::parse(sql).unwrap();
        let q = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap();
        candidates(&q, &lookup)
            .into_iter()
            .map(|c| {
                let text = Node {
                    node: Some(c.query),
                }
                .deparse()
                .unwrap();
                (c.table, c.fixed, text)
            })
            .collect()
    }

    fn one(table: &str, fixed: &[&str], sql: &str) -> Vec<(String, Vec<String>, String)> {
        vec![(
            table.to_string(),
            fixed.iter().map(|f| f.to_string()).collect(),
            sql.to_string(),
        )]
    }

    /// A LEFT JOIN read only in its own ON is a candidate, fixed on the
    /// columns that ON equates to the outer side or to a constant
    /// (`join_is_removable`, RP-1060).
    #[test]
    fn a_left_join_read_only_in_its_on_is_a_candidate() {
        let removed = "SELECT e.b FROM e";
        assert_eq!(
            removals("select e.b from e left join k on e.a = k.a"),
            one("k", &["a"], removed)
        );
        assert_eq!(
            removals("select e.b from e left join k on k.a = 3 and k.b = 'x'"),
            one("k", &["a", "b"], removed)
        );
        // Across integer widths, and over an outer expression.
        assert_eq!(
            removals("select e.b from e left join h on e.a = h.a"),
            one("h", &["a"], removed)
        );
        assert_eq!(
            removals("select e.b from e left join k on e.a + 1 = k.a"),
            one("k", &["a"], removed)
        );
        assert_eq!(
            removals("select a from e left join k using (a)"),
            one("k", &["a"], "SELECT a FROM e")
        );
        assert_eq!(
            removals("select e.b from e left join k on e.a = k.a left join h on h.a = e.a").len(),
            2
        );
    }

    /// A join something above it reads, or whose equality is no use to a
    /// unique index, is not.
    #[test]
    fn a_left_join_read_above_or_unprovable_is_not() {
        for sql in [
            "select * from e left join k on e.a = k.a",
            "select e.b, k.b from e left join k on e.a = k.a",
            "select e.b from e left join k on e.a = k.a where k.b is null",
            "select e.b from e left join k on e.a = k.a order by k.b",
            "select count(k) from e left join k on e.a = k.a",
            "select e.b from e join k on e.a = k.a",
            "select e.b from e left join k on e.a < k.a",
            "select e.b from e left join k on k.a = e.a + k.a",
            "select e.b from e left join k on (random() * 10)::int = k.a",
            // A numeric equality casts the inner column.
            "select e.b from e left join n on e.a = n.a",
            "select e.b from e left join k on k.a = 3.5",
            "select e.b from e left join k on e.a = k.a for update",
        ] {
            assert_eq!(removals(sql), Vec::new(), "{sql}");
        }
    }

    /// The query's candidates with each one's `always`.
    fn always(sql: &str) -> Vec<bool> {
        let tree = pg_query::parse(sql).unwrap();
        let q = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap();
        let lookup = |name: &str| -> Option<TableDef> {
            matches!(name, "e" | "k").then(|| {
                TableDef::new(
                    name,
                    vec![
                        secantus_pgcatalog::Column::new("a", "int4", false),
                        secantus_pgcatalog::Column::new("b", "text", false),
                    ],
                )
            })
        };
        candidates(&q, &lookup)
            .into_iter()
            .map(|c| c.always)
            .collect()
    }

    /// A view, an inlined CTE and a FROM subquery over one table are that
    /// table once pulled up, each column mapped to the table's own, and a
    /// WHERE's equality to a constant fixes its column (RP-1106).
    #[test]
    fn a_pulled_up_subquery_is_its_table() {
        crate::set_views(vec![
            ("kv".into(), "SELECT * FROM k".into()),
            ("kw".into(), "SELECT a AS x, b FROM k WHERE b = 'v1'".into()),
            ("kb".into(), "SELECT * FROM k WHERE a = 3".into()),
            ("kv2".into(), "SELECT a AS z FROM kv".into()),
            ("ks".into(), "SELECT * FROM k".into()),
        ]);
        crate::set_security_barrier_views(vec!["ks".into()]);
        let removed = "SELECT e.b FROM e";
        for sql in [
            "select e.b from e left join kv on kv.a = e.a",
            "select e.b from e left join kv2 on kv2.z = e.a",
            "select e.b from e left join (select * from k) s on s.a = e.a",
            "select e.b from e left join (select * from k) s(x, y) on s.x = e.a",
            "with c as (select * from k) select e.b from e left join c on c.a = e.a",
        ] {
            assert_eq!(removals(sql), one("k", &["a"], removed), "{sql}");
        }
        // The WHERE's equality fixes its column before the ON's do.
        for sql in [
            "select e.b from e left join kw on kw.x = e.a",
            "select e.b from e left join (select b, a from k where b = 'x') s on e.a = s.a",
        ] {
            assert_eq!(removals(sql), one("k", &["b", "a"], removed), "{sql}");
        }
        assert_eq!(
            removals("select e.b from e left join kb on kb.b = e.b"),
            one("k", &["a", "b"], removed)
        );
        assert_eq!(
            removals("select e.b from e left join (select * from k where 3 = a) s on s.b = e.b"),
            one("k", &["a", "b"], removed)
        );
        // A numeric constant casts the integer column: it fixes nothing.
        assert_eq!(
            removals("select e.b from e left join (select * from k where a = 3.5) s on s.b = e.b"),
            one("k", &["b"], removed)
        );
        for sql in [
            // A security barrier view stays a subquery.
            "select e.b from e left join ks on ks.a = e.a",
            // A column that is an expression fixes nothing.
            "select e.b from e left join (select a + 1 as a from k) s on s.a = e.a",
            "select e.b from e left join (select * from k limit 5) s on s.a = e.a",
            "select e.b from e left join (select * from k order by a) s on s.a = e.a",
            "select e.b from e left join (select distinct a from k) s on s.a = e.a",
            "select e.b from e left join (select max(a) as a from k) s on s.a = e.a",
            "select e.b from e left join (select random() as a) s on s.a = e.a",
            "select e.b from e left join (select generate_series(1, 2) as a) s on s.a = e.a",
            "select e.b from e left join (select 1 as a where false) s on s.a = e.a",
            "select e.b from e left join lateral (select e.a as a) s on true",
            "select e.b from e left join (select k.a from k join e on e.a = k.a) s on s.a = e.a",
            // MATERIALIZED, or read twice, a CTE is not inlined.
            "with c as materialized (select * from k) select e.b from e left join c on c.a = e.a",
            "with c as (select * from k) select e.b from e left join c on c.a = e.a \
             left join c c2 on c2.a = e.a",
            // What the subquery shows is read above it.
            "select s.b from e left join (select * from k) s on s.a = e.a",
            "select b from e left join (select a as b from k) s on s.b = e.a",
        ] {
            assert_eq!(removals(sql), Vec::new(), "{sql}");
        }
        crate::set_views(Vec::new());
        crate::set_security_barrier_views(Vec::new());
    }

    /// A subquery with no FROM and no WHERE is one row: its join is removed
    /// whatever its ON, with no index to ask (`remove_useless_result_rtes`,
    /// RP-1106).
    #[test]
    fn a_left_join_to_one_row_is_always_removable() {
        for sql in [
            "with k as (select 1 as a) select e.b from e left join k on k.a = e.a",
            "select e.b from e left join (select 1 as a) s on s.a = e.a",
            "select e.b from e left join (select 1 as a) s on random() > 0.5",
            "select e.b from e left join (select * from (select 1 as a) x) s on s.a = e.a",
        ] {
            assert_eq!(always(sql), vec![true], "{sql}");
        }
        for sql in [
            "select e.b, s.a from e left join (select 1 as a) s on s.a = e.a",
            "select e.b from e left join (select 1 as a) s on s.a = e.a where s.a is null",
            "select e.b from e left join (select 1 as a) s on s.a = e.a left join k on k.a = s.a",
        ] {
            assert_eq!(always(sql), Vec::<bool>::new(), "{sql}");
        }
    }
}
