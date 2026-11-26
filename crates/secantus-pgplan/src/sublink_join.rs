//! `pull_up_sublinks` for an EXPLAINed SELECT (RP-411): a WHERE that is
//! one `col IN (SELECT col FROM t)` or one `EXISTS (SELECT ... FROM t
//! WHERE t.x = outer.y)` is the semi join `convert_ANY_sublink_to_join` /
//! `convert_EXISTS_sublink_to_join` make of it, planned as a `Hash Semi
//! Join` of the outer relation's scan to the inner one's, hashed.
//!
//! Recognised here: a plain parent over one plain relation whose WHERE is
//! that sublink, alone or ANDed with conjuncts over the outer relation
//! alone (which filter its scan), over a subquery reading one plain relation, with
//! a WHERE over that relation alone, which filters its scan (IN), or a WHERE
//! that is the one correlating column equality (EXISTS). Anything else is left to the plan as written.
//!
//! Either side of an IN's comparison may be an expression over one column
//! of its relation (`a::bigint in (select a::bigint + 0 from k)`): the
//! hash clause compares the expressions, and the inner side made unique is
//! grouped by its expression (`create_unique_path`).
//!
//! Over an inner relation unique on its column, `reduce_unique_semijoins`
//! makes the semi join an inner join: `as_inner_join` writes that join, and
//! the server plans it when a unique index fixes the column.
//!
//! Otherwise only the node shape is modelled: a semi join has no planner
//! path or price here yet (`eqjoinsel_semi`, and the unique-ified inner
//! join `add_path` weighs against it), so its costs print as zeros.

use super::*;
use pg_query::protobuf::{ColumnRef, RangeVar, SelectStmt, SubLink, SubLinkType};

/// A semi join's sides and its one hash clause, as EXPLAIN prints them.
#[derive(Debug, Clone, PartialEq)]
pub struct SemiJoin {
    /// The outer relation's table and the name its scan prints (its alias).
    pub outer: (String, String),
    /// The inner relation's table and printed name.
    pub inner: (String, String),
    /// The inner relation's column the clause compares (or the one its
    /// expression reads).
    pub inner_col: String,
    /// The outer relation's column the clause compares (or the one its
    /// expression reads).
    pub outer_col: String,
    /// The outer side of an IN's comparison where it is an expression over
    /// `outer_col` (RP-411); `None`: the bare column.
    pub outer_expr: Option<KeyExpr>,
    /// The inner side's, over `inner_col`.
    pub inner_expr: Option<KeyExpr>,
    /// `(outer.col = inner.col)`.
    pub hash_cond: String,
    /// The WHERE's other conjuncts, over the outer relation alone, as its
    /// scan's `Filter` prints them (empty: the WHERE is the sublink).
    pub outer_quals: Vec<(String, Option<String>)>,
    /// The conjuncts of `outer_quals` that do not lower to the scan's
    /// filter, comparisons of expressions with constants the scan runs per
    /// row (RP-411, as `ExplainOptions::scan_expr_quals`).
    pub outer_exprs: Vec<ExprQual>,
    /// An IN subquery's own WHERE, over the inner relation alone, lowered
    /// to the inner scan's filter (RP-411; empty: none).
    pub inner_filter: Document,
    /// That WHERE as the inner scan's `Filter` prints it.
    pub inner_quals: Vec<(String, Option<String>)>,
    /// The EXPLAIN of the inner join `reduce_unique_semijoins` makes of
    /// it when the inner relation is unique on `inner_col` (RP-411):
    /// `outer JOIN inner ON outer.col = inner.col`, the select list read
    /// from the outer side. Planned in `plan_explain`; the server, which
    /// knows the unique indexes, chooses it.
    pub inner_join: Option<Box<Statement>>,
}

/// One side of a semi join's clause that is an expression over one column
/// of its relation (RP-411).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyExpr {
    /// As EXPLAIN prints it, qualified: `((k.a)::bigint + 0)`.
    pub text: String,
    /// The operators it evaluates a row (`cost_qual_eval`).
    pub ops: u32,
    /// Its result type's name (`int8`).
    pub pg_type: String,
    /// As written, its columns qualified by the relation's printed name.
    pub node: pg_query::protobuf::Node,
}

/// `n`, every column of which is `def`'s (printed as `name`), as a
/// [`KeyExpr`] with the one column it reads: an integer or string
/// expression `expr_side` prices, reading exactly one column and
/// evaluating at least one operator (a bare column, or a cast PostgreSQL
/// strips, is not one).
fn key_expr(n: &pg_query::protobuf::Node, name: &str, def: &TableDef) -> Option<(String, KeyExpr)> {
    if !outer_only(n, name, def) {
        return None;
    }
    let side = expr_side(n, def)?;
    let [field] = side.fields.as_slice() else {
        return None;
    };
    if side.operators == 0 {
        return None;
    }
    let col = def
        .columns
        .iter()
        .find(|c| c.field() == *field)?
        .name
        .clone();
    let mut shown = def.clone();
    shown.name = name.to_string();
    let text = ruleutils::explain_qualified_expr(n, &shown)?;
    let ty = plan_expression_node(n, def).ok()?;
    let pg_type = canonical_type(column_expr_type(&ty))?;
    let mut node = n.clone();
    qualify(&mut node, name);
    Some((
        col,
        KeyExpr {
            text,
            ops: side.operators,
            pg_type,
            node,
        },
    ))
}

/// A type's name as `pg_type` names it (`bigint` -> `int8`).
fn canonical_type(t: &str) -> Option<String> {
    Some(pgtypes::name_of_oid(pgtypes::oid_of_name(t)?)?.to_string())
}

/// A side of an IN's comparison: a bare column of `sides[0]`, or an
/// expression over one ([`key_expr`]).
fn key_side(
    n: &pg_query::protobuf::Node,
    sides: &[(&str, &TableDef)],
) -> Option<(String, Option<KeyExpr>)> {
    match resolve(n, sides) {
        Some((0, col)) => Some((col, None)),
        Some(_) => None,
        None if column_parts(n).is_some() => None,
        None => {
            let (col, e) = key_expr(n, sides[0].0, sides[0].1)?;
            Some((col, Some(e)))
        }
    }
}

fn plain(s: &SelectStmt) -> bool {
    s.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && s.with_clause.is_none()
        && s.group_clause.is_empty()
        && s.having_clause.is_none()
        && s.distinct_clause.is_empty()
        && s.limit_count.is_none()
        && s.limit_offset.is_none()
        && s.sort_clause.is_empty()
        && s.window_clause.is_empty()
        && s.locking_clause.is_empty()
        && s.into_clause.is_none()
        && s.values_lists.is_empty()
}

fn one_relation(s: &SelectStmt) -> Option<&RangeVar> {
    let [item] = s.from_clause.as_slice() else {
        return None;
    };
    match item.node.as_ref() {
        Some(N::RangeVar(rv))
            if rv.inh
                && rv.schemaname.is_empty()
                && rv.alias.as_ref().is_none_or(|a| a.colnames.is_empty()) =>
        {
            Some(rv)
        }
        _ => None,
    }
}

fn shown_name(rv: &RangeVar) -> String {
    rv.alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_else(|| rv.relname.clone())
}

fn column_parts(n: &pg_query::protobuf::Node) -> Option<Vec<String>> {
    let Some(N::ColumnRef(ColumnRef { fields, .. })) = n.node.as_ref() else {
        return None;
    };
    fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A column reference resolved against `sides` (`(printed name, table)`):
/// `(side index, column)`. An unqualified name is the first side's that has
/// it, as the subquery's own relation shadows the outer one.
fn resolve(n: &pg_query::protobuf::Node, sides: &[(&str, &TableDef)]) -> Option<(usize, String)> {
    let parts = column_parts(n)?;
    match parts.as_slice() {
        [col] => {
            let mut hits = sides
                .iter()
                .enumerate()
                .filter(|(_, (_, d))| user_column(d, col).is_some());
            let (i, _) = hits.next()?;
            Some((i, col.clone()))
        }
        [rel, col] => {
            let i = sides.iter().position(|(name, _)| name == rel)?;
            user_column(sides[i].1, col)?;
            Some((i, col.clone()))
        }
        _ => None,
    }
}

fn user_column<'a>(def: &'a TableDef, col: &str) -> Option<&'a Column> {
    def.columns.iter().find(|c| c.name == col)
}

/// The WHERE's one sublink and its other AND conjuncts, which must not
/// hold a sublink themselves.
fn split_where(w: &pg_query::protobuf::Node) -> Option<(&SubLink, Vec<pg_query::protobuf::Node>)> {
    let args: Vec<&pg_query::protobuf::Node> = match w.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            b.args.iter().collect()
        }
        _ => vec![w],
    };
    let mut link = None;
    let mut rest = Vec::new();
    for a in args {
        match a.node.as_ref() {
            Some(N::SubLink(l)) if link.is_none() => link = Some(l.as_ref()),
            _ if crate::filter_sublink::has_sublink(a) => return None,
            _ => rest.push(a.clone()),
        }
    }
    Some((link?, rest))
}

/// `rest` ANDed back into one WHERE.
fn and_of(mut rest: Vec<pg_query::protobuf::Node>) -> Option<pg_query::protobuf::Node> {
    match rest.len() {
        0 => None,
        1 => rest.pop(),
        _ => Some(pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                args: rest,
                ..Default::default()
            }))),
        }),
    }
}

/// Whether every column `n` names is the outer relation's.
fn outer_only(n: &pg_query::protobuf::Node, name: &str, def: &TableDef) -> bool {
    let Some(e) = n.node.as_ref() else {
        return false;
    };
    e.nodes().into_iter().all(|(r, _, _, _)| match r {
        pg_query::NodeRef::ColumnRef(c) => {
            let node = pg_query::protobuf::Node {
                node: Some(N::ColumnRef(c.clone())),
            };
            resolve(&node, &[(name, def)]).is_some()
        }
        _ => true,
    })
}

fn is_equals(names: &[pg_query::protobuf::Node]) -> bool {
    matches!(
        names,
        [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "=")
    )
}

/// The semi join `query`'s WHERE pulls up into, if it is one.
pub(crate) fn semi_join_of(
    query: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<Box<SemiJoin>> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    if !plain(s) {
        return None;
    }
    let outer_rv = one_relation(s)?;
    let outer_def = lookup(&outer_rv.relname)?;
    let outer_name = shown_name(outer_rv);
    let (link, rest) = split_where(s.where_clause.as_deref()?)?;
    if !rest.iter().all(|n| outer_only(n, &outer_name, &outer_def)) {
        return None;
    }
    // A conjunct the scan's filter does not hold whole is priced as a
    // comparison of an expression with a constant, per row; any other is
    // not modelled.
    let mut outer_exprs = Vec::new();
    for p in &rest {
        match lower_where_nonvolatile(p, &outer_def, &[]) {
            Ok(_) => {}
            Err(Error::Unsupported(_)) => outer_exprs.push(expr_qual(p, &outer_def)?),
            Err(_) => return None,
        }
    }
    let outer_quals = match and_of(rest) {
        Some(w) => ruleutils::explain_conjuncts(&w, &outer_def)?,
        None => Vec::new(),
    };
    let Some(N::SelectStmt(sub)) = link.subselect.as_deref()?.node.as_ref() else {
        return None;
    };
    if !plain(sub) {
        return None;
    }
    let inner_rv = one_relation(sub)?;
    let inner_def = lookup(&inner_rv.relname)?;
    let inner_name = shown_name(inner_rv);
    if inner_name == outer_name {
        return None;
    }
    // Inner first: within the subquery its own relation shadows the outer.
    let sides = [
        (inner_name.as_str(), &inner_def),
        (outer_name.as_str(), &outer_def),
    ];
    let mut inner_filter = Document::new();
    let mut inner_quals = Vec::new();
    let (mut outer_expr, mut inner_expr) = (None, None);
    let (outer_col, inner_col) = match SubLinkType::try_from(link.sub_link_type).ok()? {
        SubLinkType::AnySublink => {
            // `IN` leaves the operator name empty; `= ANY` names it.
            if !(link.oper_name.is_empty() || is_equals(&link.oper_name)) {
                return None;
            }
            // The subquery's own WHERE, over its relation alone and held
            // whole by the scan's filter, filters the inner scan
            // (`pull_up_sublinks` keeps it below the join).
            if let Some(w) = sub.where_clause.as_deref() {
                if crate::filter_sublink::has_sublink(w) || !outer_only(w, &inner_name, &inner_def)
                {
                    return None;
                }
                // Its columns as the scan names them: an alias's qualifier
                // (`e2.b`) is the relation's own.
                let w = unqualified(w, &inner_name)?;
                inner_filter = lower_where_nonvolatile(&w, &inner_def, &[]).ok()?;
                inner_quals = ruleutils::explain_conjuncts(&w, &inner_def)?;
            }
            let [t] = sub.target_list.as_slice() else {
                return None;
            };
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                return None;
            };
            let (inner_col, ie) = key_side(rt.val.as_deref()?, &sides[..1])?;
            let (outer, oe) = key_side(link.testexpr.as_deref()?, &[sides[1]])?;
            (outer_expr, inner_expr) = (oe, ie);
            (outer, inner_col)
        }
        SubLinkType::ExistsSublink => {
            let Some(N::AExpr(e)) = sub.where_clause.as_deref()?.node.as_ref() else {
                return None;
            };
            if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !is_equals(&e.name) {
                return None;
            }
            let l = resolve(e.lexpr.as_deref()?, &sides)?;
            let r = resolve(e.rexpr.as_deref()?, &sides)?;
            match (l, r) {
                ((0, i), (1, o)) | ((1, o), (0, i)) => (o, i),
                _ => return None,
            }
        }
        _ => return None,
    };
    // Two types would print the coercion PostgreSQL adds; not modelled.
    let side_type = |e: &Option<KeyExpr>, def: &TableDef, col: &str| match e {
        Some(e) => Some(e.pg_type.clone()),
        None => canonical_type(&user_column(def, col)?.pg_type),
    };
    if side_type(&outer_expr, &outer_def, &outer_col)?
        != side_type(&inner_expr, &inner_def, &inner_col)?
    {
        return None;
    }
    let shown = |e: &Option<KeyExpr>, rel: &str, col: &str| match e {
        Some(e) => e.text.clone(),
        None => format!("{rel}.{col}"),
    };
    Some(Box::new(SemiJoin {
        hash_cond: format!(
            "({} = {})",
            shown(&outer_expr, &outer_name, &outer_col),
            shown(&inner_expr, &inner_name, &inner_col)
        ),
        outer: (outer_rv.relname.clone(), outer_name),
        inner: (inner_rv.relname.clone(), inner_name),
        inner_col,
        outer_col,
        outer_expr,
        inner_expr,
        outer_quals,
        outer_exprs,
        inner_filter,
        inner_quals,
        inner_join: None,
    }))
}

/// Every one-part column reference in `n` qualified by `rel`.
fn qualify(n: &mut pg_query::protobuf::Node, rel: &str) {
    let Some(e) = n.node.as_mut() else { return };
    match e {
        N::ColumnRef(c) => {
            if let [f] = c.fields.as_slice() {
                if matches!(f.node, Some(N::String(_))) {
                    c.fields.insert(
                        0,
                        pg_query::protobuf::Node {
                            node: Some(string(rel)),
                        },
                    );
                }
            }
        }
        N::AExpr(x) => {
            for side in [x.lexpr.as_deref_mut(), x.rexpr.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                qualify(side, rel);
            }
        }
        N::BoolExpr(b) => b.args.iter_mut().for_each(|a| qualify(a, rel)),
        N::TypeCast(t) => {
            if let Some(a) = t.arg.as_deref_mut() {
                qualify(a, rel);
            }
        }
        N::NullTest(t) => {
            if let Some(a) = t.arg.as_deref_mut() {
                qualify(a, rel);
            }
        }
        N::FuncCall(f) => f.args.iter_mut().for_each(|a| qualify(a, rel)),
        N::List(l) => l.items.iter_mut().for_each(|a| qualify(a, rel)),
        _ => {}
    }
}

/// Does `n` hold a one-part column reference?
fn has_bare_column(n: &pg_query::protobuf::Node) -> bool {
    let mut n = n.clone();
    crate::walk_expr(&mut n, &mut |x| match x.node.as_ref() {
        Some(N::ColumnRef(c)) if c.fields.len() == 1 => Err(Error::Parse(String::new())),
        _ => Ok(()),
    })
    .is_err()
}

/// `n` with every `rel.col` reference made a bare `col`.
fn unqualified(n: &pg_query::protobuf::Node, rel: &str) -> Option<pg_query::protobuf::Node> {
    let mut out = n.clone();
    crate::walk_expr(&mut out, &mut |x| {
        if let Some(N::ColumnRef(c)) = x.node.as_mut() {
            let named = matches!(
                c.fields.as_slice(),
                [r, f] if matches!(r.node.as_ref(), Some(N::String(s)) if s.sval == rel)
                    && matches!(f.node, Some(N::String(_)))
            );
            if named {
                c.fields.remove(0);
            }
        }
        Ok(())
    })
    .ok()?;
    Some(out)
}

fn column_ref(fields: Vec<N>) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(ColumnRef {
            fields: fields
                .into_iter()
                .map(|f| pg_query::protobuf::Node { node: Some(f) })
                .collect(),
            location: -1,
        })),
    }
}

fn string(s: &str) -> N {
    N::String(pg_query::protobuf::String { sval: s.into() })
}

/// `query` with its WHERE sublink `sj` made the inner join
/// `reduce_unique_semijoins` leaves of a semi join whose inner side is
/// unique (RP-411): FROM both relations, the hash clause as its WHERE, and
/// each select-list column read from the outer side. `None` when the select
/// list has anything but `*` and the outer side's own columns.
pub(crate) fn as_inner_join(query: &N, sj: &SemiJoin) -> Option<N> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    // An expression is unique by no index on its column
    // (`innerrel_is_unique` matches an index's own columns).
    if sj.inner_expr.is_some() {
        return None;
    }
    let (link, rest) = split_where(s.where_clause.as_deref()?)?;
    let Some(N::SelectStmt(sub)) = link.subselect.as_deref()?.node.as_ref() else {
        return None;
    };
    let inner_rv = one_relation(sub)?.clone();
    let outer = sj.outer.1.as_str();
    let mut out = s.clone();
    for t in &mut out.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_mut() else {
            return None;
        };
        let parts = rt.val.as_deref().and_then(|v| match v.node.as_ref() {
            Some(N::ColumnRef(c)) => Some(c.fields.clone()),
            _ => None,
        })?;
        let qualified = match parts.as_slice() {
            [f] if matches!(f.node, Some(N::AStar(_))) => {
                vec![string(outer), N::AStar(pg_query::protobuf::AStar {})]
            }
            [f] => match f.node.as_ref() {
                Some(N::String(c)) => vec![string(outer), string(&c.sval)],
                _ => return None,
            },
            [rel, _] if matches!(rel.node.as_ref(), Some(N::String(r)) if r.sval == outer) => {
                parts.iter().filter_map(|p| p.node.clone()).collect()
            }
            _ => return None,
        };
        rt.val = Some(Box::new(column_ref(qualified)));
    }
    out.from_clause.push(pg_query::protobuf::Node {
        node: Some(N::RangeVar(inner_rv)),
    });
    let side = |r: &str, c: &str| Box::new(column_ref(vec![string(r), string(c)]));
    // The outer relation's conjuncts name its columns bare; beside the
    // inner relation a bare name may be ambiguous.
    let mut conjuncts: Vec<pg_query::protobuf::Node> = rest
        .into_iter()
        .map(|mut n| {
            qualify(&mut n, outer);
            n
        })
        .collect();
    conjuncts.push(pg_query::protobuf::Node {
        node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: pg_query::protobuf::AExprKind::AexprOp as i32,
            name: vec![pg_query::protobuf::Node {
                node: Some(string("=")),
            }],
            lexpr: Some(match &sj.outer_expr {
                Some(e) => Box::new(e.node.clone()),
                None => side(outer, &sj.outer_col),
            }),
            rexpr: Some(side(&sj.inner.1, &sj.inner_col)),
            location: -1,
        }))),
    });
    // An IN subquery's own WHERE stays a qual of the inner relation; an
    // EXISTS's is the correlating equality written above.
    let any = link.sub_link_type == SubLinkType::AnySublink as i32;
    if let Some(w) = sub.where_clause.as_deref().filter(|_| any) {
        let mut w = w.clone();
        qualify(&mut w, &sj.inner.1);
        conjuncts.push(w);
    }
    out.where_clause = and_of(conjuncts).map(Box::new);
    Some(N::SelectStmt(out))
}

/// `NOT EXISTS (SELECT .. FROM inner WHERE inner.col = outer.col)` as the
/// anti join `convert_EXISTS_sublink_to_join` pulls it up into (RP-980),
/// written as the LEFT join PostgreSQL reduces to the same JOIN_ANTI:
/// `outer LEFT JOIN inner ON outer.col = inner.col WHERE inner.col IS
/// NULL`, the select list read from the outer side. `None` for any other
/// shape.
pub(crate) fn anti_join_of(query: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Option<N> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    // The WHERE with its `NOT EXISTS` read as `EXISTS`, for the semi join's
    // resolution of the clause and its sides.
    let mut exists = s.clone();
    let args: Vec<pg_query::protobuf::Node> = match s.where_clause.as_deref()?.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            b.args.clone()
        }
        _ => vec![s.where_clause.as_deref()?.clone()],
    };
    let mut found = false;
    let mut conjuncts = Vec::new();
    for a in args {
        match a.node.as_ref() {
            Some(N::BoolExpr(b))
                if !found && b.boolop == pg_query::protobuf::BoolExprType::NotExpr as i32 =>
            {
                let [inner] = b.args.as_slice() else {
                    return None;
                };
                let Some(N::SubLink(l)) = inner.node.as_ref() else {
                    return None;
                };
                if l.sub_link_type != SubLinkType::ExistsSublink as i32 {
                    return None;
                }
                found = true;
                conjuncts.push(inner.clone());
            }
            _ => conjuncts.push(a),
        }
    }
    // An outer conjunct beside it stays the WHERE's, over the outer
    // relation, which restricts the anti join's outer scan (RP-1027).
    if !found {
        return None;
    }
    exists.where_clause = and_of(conjuncts).map(Box::new);
    let exists = N::SelectStmt(exists);
    let sj = semi_join_of(&exists, lookup)?;
    let N::SelectStmt(mut joined) = as_inner_join(&exists, &sj)? else {
        return None;
    };
    // `as_inner_join` ends the WHERE with the equality; it is the ON.
    let mut rest = match joined.where_clause.take().map(|w| *w) {
        Some(pg_query::protobuf::Node {
            node: Some(N::BoolExpr(b)),
        }) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => b.args,
        Some(w) => vec![w],
        None => return None,
    };
    let on = rest.pop()?;
    // Each outer conjunct names its columns by the outer relation now
    // (`qualify`); one it could not reach would be ambiguous beside the
    // inner relation, or read it: not rewritten.
    if rest.iter().any(has_bare_column) {
        return None;
    }
    let [outer_rv, inner_rv] = std::mem::take(&mut joined.from_clause).try_into().ok()?;
    joined.from_clause = vec![pg_query::protobuf::Node {
        node: Some(N::JoinExpr(Box::new(pg_query::protobuf::JoinExpr {
            jointype: pg_query::protobuf::JoinType::JoinLeft as i32,
            larg: Some(Box::new(outer_rv)),
            rarg: Some(Box::new(inner_rv)),
            quals: Some(Box::new(on)),
            ..Default::default()
        }))),
    }];
    rest.push(pg_query::protobuf::Node {
        node: Some(N::NullTest(Box::new(pg_query::protobuf::NullTest {
            arg: Some(Box::new(column_ref(vec![
                string(&sj.inner.1),
                string(&sj.inner_col),
            ]))),
            nulltesttype: pg_query::protobuf::NullTestType::IsNull as i32,
            location: -1,
            ..Default::default()
        }))),
    });
    joined.where_clause = and_of(rest).map(Box::new);
    Some(N::SelectStmt(joined))
}

#[cfg(test)]
mod rp980_anti_join {
    use super::*;

    fn rewritten(sql: &str) -> Option<String> {
        let def = TableDef::new(
            "e",
            vec![
                secantus_pgcatalog::Column::new("a", "int4", false),
                secantus_pgcatalog::Column::new("b", "text", false),
            ],
        );
        let tree = pg_query::parse(sql).unwrap();
        let q = tree.protobuf.stmts[0].stmt.as_ref()?.node.clone()?;
        let out = anti_join_of(&q, &|_| Some(def.clone()))?;
        pg_query::protobuf::Node { node: Some(out) }.deparse().ok()
    }

    /// A NOT EXISTS over a column equality is the LEFT join PostgreSQL
    /// reduces to the same anti join (RP-980); an outer conjunct beside
    /// it stays the WHERE's, over the outer relation (RP-1027).
    #[test]
    fn a_not_exists_is_the_left_join_anti_form() {
        assert_eq!(
            rewritten("select * from e where not exists (select 1 from e e2 where e2.a = e.a)")
                .as_deref(),
            Some("SELECT e.* FROM e LEFT JOIN e e2 ON e.a = e2.a WHERE e2.a IS NULL")
        );
        assert_eq!(
            rewritten(
                "select * from e where a > 5 and not exists (select 1 from e e2 where e2.a = e.a)"
            )
            .as_deref(),
            Some("SELECT e.* FROM e LEFT JOIN e e2 ON e.a = e2.a WHERE e.a > 5 AND e2.a IS NULL")
        );
        // A column `qualify` does not reach would be ambiguous beside the
        // inner relation: not rewritten.
        assert_eq!(
            rewritten(
                "select * from e where coalesce(a, 0) > 5 \
                 and not exists (select 1 from e e2 where e2.a = e.a)"
            ),
            None
        );
        assert_eq!(
            rewritten("select * from e where exists (select 1 from e e2 where e2.a = e.a)"),
            None
        );
    }
}

#[cfg(test)]
mod rp411_expression_keys {
    use super::*;

    fn semi(sql: &str) -> Option<Box<SemiJoin>> {
        let def = |name: &str| {
            TableDef::new(
                name,
                vec![
                    secantus_pgcatalog::Column::new("a", "int4", false),
                    secantus_pgcatalog::Column::new("b", "text", false),
                ],
            )
        };
        let tree = pg_query::parse(sql).unwrap();
        let q = tree.protobuf.stmts[0].stmt.as_ref()?.node.clone()?;
        semi_join_of(&q, &|t| Some(def(t)))
    }

    /// An IN comparing expressions over one column of each side hashes
    /// the expressions, as PostgreSQL 15.19 prints them (RP-411); it is
    /// never the inner join of a unique inner column.
    #[test]
    fn an_in_comparing_expressions_hashes_them() {
        let sj = semi("select * from e where a::bigint in (select a::bigint + 0 from k)").unwrap();
        assert_eq!(sj.hash_cond, "((e.a)::bigint = ((k.a)::bigint + 0))");
        assert_eq!((sj.outer_col.as_str(), sj.inner_col.as_str()), ("a", "a"));
        let (o, i) = (
            sj.outer_expr.as_ref().unwrap(),
            sj.inner_expr.as_ref().unwrap(),
        );
        assert_eq!((o.ops, o.pg_type.as_str()), (1, "int8"));
        assert_eq!(
            (i.ops, i.pg_type.as_str(), i.text.as_str()),
            (2, "int8", "((k.a)::bigint + 0)")
        );
        let tree =
            pg_query::parse("select * from e where a::bigint in (select a::bigint + 0 from k)")
                .unwrap();
        let q = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap();
        assert_eq!(as_inner_join(&q, &sj), None);
        let sj = semi("select * from e x where upper(x.b) in (select lower(b) from k)").unwrap();
        assert_eq!(sj.hash_cond, "(upper(x.b) = lower(k.b))");
    }

    /// One side bare, the other an expression of the same type; an alias's
    /// qualifier in the subquery's WHERE is the scan's own column.
    #[test]
    fn one_side_may_be_a_bare_column() {
        let sj =
            semi("select * from e where a in (select a + 0 from e e2 where e2.b = 'v3')").unwrap();
        assert_eq!(sj.hash_cond, "(e.a = (e2.a + 0))");
        assert!(sj.outer_expr.is_none());
        assert_eq!(
            sj.inner_quals,
            vec![("(b = 'v3'::text)".to_string(), Some("b".to_string()))]
        );
    }

    /// Across types PostgreSQL compares through a cross-type operator,
    /// not modelled; an expression over two columns, or none, is declined.
    #[test]
    fn other_shapes_are_declined() {
        assert_eq!(
            semi("select * from e where a::bigint in (select a from k)"),
            None
        );
        assert_eq!(
            semi("select * from e where a + b::int in (select a from k)"),
            None
        );
        assert_eq!(
            semi("select * from e where a in (select 1 + 0 from k)"),
            None
        );
    }
}
