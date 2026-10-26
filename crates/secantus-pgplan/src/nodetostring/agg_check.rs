//! `parse_agg.c`'s grouping work and `parseCheckAggregates`: GROUPING SETS
//! (flattening, transformation and expansion), `GROUPING()`, and the check
//! of ungrouped columns, which records the primary keys a query's grouping
//! relies on in `constraintDeps`.

use super::nodefuncs::children;
use super::nodes::*;
use super::out::equal;
use super::parse::{Analyzer, ExprKind};
use super::{pg_error, unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

/// `GroupingSetKind` (PostgreSQL's numbering; the protobuf's is one more).
pub(crate) const GROUPING_SET_EMPTY: i32 = 0;
pub(crate) const GROUPING_SET_SIMPLE: i32 = 1;
pub(crate) const GROUPING_SET_ROLLUP: i32 = 2;
pub(crate) const GROUPING_SET_CUBE: i32 = 3;
pub(crate) const GROUPING_SET_SETS: i32 = 4;

/// A GROUP BY item after `flatten_grouping_sets`: an expression, a list
/// (an implicit row's members) or a grouping set.
#[derive(Clone, Debug)]
pub(crate) enum Flat {
    Expr(Box<pb::Node>),
    List(Vec<Flat>),
    Set {
        kind: i32,
        content: Vec<Flat>,
        location: i32,
    },
}

fn leftmost(a: i32, b: i32) -> i32 {
    if a < 0 {
        b
    } else if b < 0 {
        a
    } else {
        a.min(b)
    }
}

/// `exprLocation` of a raw parse node, for the nodes a GROUP BY item is.
pub(crate) fn raw_location(n: &pb::Node) -> i32 {
    let list = |l: &[pb::Node]| l.iter().fold(-1, |acc, x| leftmost(acc, raw_location(x)));
    let opt = |o: &Option<Box<pb::Node>>| o.as_deref().map_or(-1, raw_location);
    match n.node.as_ref() {
        Some(N::ColumnRef(c)) => c.location,
        Some(N::AConst(c)) => c.location,
        Some(N::ParamRef(p)) => p.location,
        Some(N::AExpr(a)) => leftmost(a.location, opt(&a.lexpr)),
        Some(N::FuncCall(f)) => leftmost(f.location, list(&f.args)),
        Some(N::TypeCast(t)) => leftmost(t.location, opt(&t.arg)),
        Some(N::CollateClause(c)) => leftmost(c.location, opt(&c.arg)),
        Some(N::BoolExpr(b)) => leftmost(b.location, list(&b.args)),
        Some(N::NullTest(t)) => leftmost(t.location, opt(&t.arg)),
        Some(N::BooleanTest(t)) => leftmost(t.location, opt(&t.arg)),
        Some(N::SubLink(s)) => leftmost(s.location, opt(&s.testexpr)),
        Some(N::RowExpr(r)) => r.location,
        Some(N::AArrayExpr(a)) => a.location,
        Some(N::CaseExpr(c)) => c.location,
        Some(N::CoalesceExpr(c)) => c.location,
        Some(N::MinMaxExpr(m)) => m.location,
        Some(N::SqlvalueFunction(s)) => s.location,
        Some(N::AIndirection(i)) => opt(&i.arg),
        Some(N::GroupingSet(g)) => g.location,
        Some(N::List(l)) => list(&l.items),
        _ => -1,
    }
}

fn flat_location(f: &Flat) -> i32 {
    match f {
        Flat::Expr(n) => raw_location(n),
        Flat::List(l) => l.iter().fold(-1, |acc, x| leftmost(acc, flat_location(x))),
        Flat::Set { location, .. } => *location,
    }
}

/// `flatten_grouping_sets`. `None` is NIL.
fn flatten(n: &pb::Node, toplevel: bool, has_sets: &mut bool) -> Option<Flat> {
    match n.node.as_ref() {
        Some(N::RowExpr(r)) if r.row_format == pb::CoercionForm::CoerceImplicitCast as i32 => {
            flatten_list(&r.args, false, &mut false)
        }
        Some(N::GroupingSet(g)) => {
            *has_sets = true;
            let kind = g.kind - 1;
            if toplevel && kind == GROUPING_SET_EMPTY {
                return None;
            }
            let mut result = Vec::new();
            for n1 in &g.content {
                let n2 = flatten(n1, false, &mut false);
                let n1_is_sets = matches!(n1.node.as_ref(),
                    Some(N::GroupingSet(s)) if s.kind - 1 == GROUPING_SET_SETS);
                match n2 {
                    Some(Flat::List(items)) if n1_is_sets => result.extend(items),
                    None if n1_is_sets => {}
                    Some(x) => result.push(x),
                    None => result.push(Flat::List(Vec::new())),
                }
            }
            if toplevel || kind != GROUPING_SET_SETS {
                Some(Flat::Set {
                    kind,
                    content: result,
                    location: g.location,
                })
            } else {
                Some(Flat::List(result))
            }
        }
        Some(N::List(l)) => flatten_list(&l.items, toplevel, has_sets),
        _ => Some(Flat::Expr(Box::new(n.clone()))),
    }
}

fn flatten_list(items: &[pb::Node], toplevel: bool, has_sets: &mut bool) -> Option<Flat> {
    let mut result = Vec::new();
    for item in items {
        match flatten(item, toplevel, has_sets) {
            None => {}
            Some(Flat::List(l)) => result.extend(l),
            Some(x) => result.push(x),
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(Flat::List(result))
    }
}

/// `expand_groupingset_node`.
fn expand_node(gs: &GroupingSet) -> Vec<Vec<i32>> {
    let simple = |c: &GroupingSetContent| -> Vec<i32> {
        match c {
            GroupingSetContent::Set(s) => s
                .content
                .iter()
                .filter_map(|x| match x {
                    GroupingSetContent::Ref(r) => Some(*r),
                    GroupingSetContent::Set(_) => None,
                })
                .collect(),
            GroupingSetContent::Ref(r) => vec![*r],
        }
    };
    match gs.kind {
        GROUPING_SET_EMPTY => vec![Vec::new()],
        GROUPING_SET_SIMPLE => vec![gs
            .content
            .iter()
            .filter_map(|x| match x {
                GroupingSetContent::Ref(r) => Some(*r),
                GroupingSetContent::Set(_) => None,
            })
            .collect()],
        GROUPING_SET_ROLLUP => {
            let mut result = Vec::new();
            let mut size = gs.content.len();
            while size > 0 {
                let mut cur = Vec::new();
                for c in gs.content.iter().take(size) {
                    cur.extend(simple(c));
                }
                result.push(cur);
                size -= 1;
            }
            result.push(Vec::new());
            result
        }
        GROUPING_SET_CUBE => {
            let n = gs.content.len();
            (0u32..(1u32 << n))
                .map(|i| {
                    let mut cur = Vec::new();
                    for (b, c) in gs.content.iter().enumerate() {
                        if i & (1 << b) != 0 {
                            cur.extend(simple(c));
                        }
                    }
                    cur
                })
                .collect()
        }
        _ => gs
            .content
            .iter()
            .flat_map(|c| match c {
                GroupingSetContent::Set(s) => expand_node(s),
                GroupingSetContent::Ref(r) => vec![vec![*r]],
            })
            .collect(),
    }
}

/// `list_union_int`: `a` then the members of `b` it lacks.
fn union_int(a: &[i32], b: &[i32]) -> Vec<i32> {
    let mut out = a.to_vec();
    for x in b {
        if !out.contains(x) {
            out.push(*x);
        }
    }
    out
}

/// `expand_grouping_sets`; empty when over `limit`.
pub(crate) fn expand_grouping_sets(
    sets: &[GroupingSet],
    distinct: bool,
    limit: usize,
) -> Vec<Vec<i32>> {
    if sets.is_empty() {
        return Vec::new();
    }
    let mut expanded = Vec::new();
    let mut numsets: f64 = 1.0;
    for gs in sets {
        let cur = expand_node(gs);
        numsets *= cur.len() as f64;
        if numsets > limit as f64 {
            return Vec::new();
        }
        expanded.push(cur);
    }
    let mut result: Vec<Vec<i32>> = expanded[0].iter().map(|l| union_int(&[], l)).collect();
    for p in &expanded[1..] {
        let mut next = Vec::new();
        for q in &result {
            for l in p {
                next.push(union_int(q, l));
            }
        }
        result = next;
    }
    if !distinct || result.len() < 2 {
        // Only the number of sets and their members are used from here, so
        // the order among sets of one length does not matter.
        result.sort_by_key(Vec::len);
    } else {
        for r in result.iter_mut() {
            r.sort_unstable();
        }
        result.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        result.dedup();
    }
    result
}

impl<'a> Analyzer<'a> {
    /// `transformGroupClause`: (the flat clause, the grouping sets).
    pub fn transform_group_clause(
        &mut self,
        grouplist: &[pb::Node],
        _allow_sets: bool,
        tlist: &mut Vec<TargetEntry>,
        sort_clause: &[SortGroupClause],
        kind: ExprKind,
        sql99: bool,
    ) -> R<(Vec<SortGroupClause>, Vec<GroupingSet>)> {
        let mut has_sets = false;
        let mut flat = match flatten_list(grouplist, true, &mut has_sets) {
            Some(Flat::List(l)) => l,
            Some(x) => vec![x],
            None => Vec::new(),
        };
        if flat.is_empty() && has_sets {
            let loc = grouplist
                .iter()
                .fold(-1, |acc, x| leftmost(acc, raw_location(x)));
            flat.push(Flat::Set {
                kind: GROUPING_SET_EMPTY,
                content: Vec::new(),
                location: loc,
            });
        }
        let mut result: Vec<SortGroupClause> = Vec::new();
        let mut gsets = Vec::new();
        let mut seen_local: Vec<u32> = Vec::new();
        for g in &flat {
            match g {
                Flat::Set {
                    kind: GROUPING_SET_EMPTY,
                    location,
                    ..
                } => gsets.push(GroupingSet {
                    kind: GROUPING_SET_EMPTY,
                    content: Vec::new(),
                    location: *location,
                }),
                Flat::Set { .. } => gsets.push(self.transform_grouping_set(
                    &mut result,
                    g,
                    tlist,
                    sort_clause,
                    kind,
                    sql99,
                    true,
                )?),
                Flat::Expr(e) => {
                    let r = self.transform_group_clause_expr(
                        &mut result,
                        Some(&seen_local),
                        e,
                        tlist,
                        sort_clause,
                        kind,
                        sql99,
                        true,
                    )?;
                    if r > 0 {
                        seen_local.push(r);
                        if has_sets {
                            gsets.push(GroupingSet {
                                kind: GROUPING_SET_SIMPLE,
                                content: vec![GroupingSetContent::Ref(r as i32)],
                                location: raw_location(e),
                            });
                        }
                    }
                }
                Flat::List(_) => return unsupported("a nested GROUP BY list"),
            }
        }
        Ok((result, gsets))
    }

    /// `transformGroupClauseExpr`: the item's sortgroupref, 0 for a local
    /// duplicate.
    #[allow(clippy::too_many_arguments)]
    fn transform_group_clause_expr(
        &mut self,
        flat: &mut Vec<SortGroupClause>,
        seen_local: Option<&[u32]>,
        gexpr: &pb::Node,
        tlist: &mut Vec<TargetEntry>,
        sort_clause: &[SortGroupClause],
        kind: ExprKind,
        sql99: bool,
        toplevel: bool,
    ) -> R<u32> {
        let idx = if sql99 {
            self.find_targetlist_entry_sql99(gexpr, tlist, kind)?
        } else {
            self.find_targetlist_entry_sql92(gexpr, tlist, kind)?
        };
        let mut found = false;
        let r = tlist[idx].ressortgroupref;
        if r > 0 {
            if seen_local.is_some_and(|s| s.contains(&r)) {
                return Ok(0);
            }
            if super::clause::target_is_in_sort_list(self.cat, &tlist[idx], 0, flat) {
                return Ok(r);
            }
            if let Some(sc) = sort_clause.iter().find(|s| s.tle_sort_group_ref == r) {
                let mut grpc = sc.clone();
                if !toplevel {
                    grpc.nulls_first = false;
                }
                flat.push(grpc);
                found = true;
            }
        }
        if !found {
            self.add_target_to_group_list(idx, flat, tlist)?;
        }
        Ok(tlist[idx].ressortgroupref)
    }

    /// `transformGroupingSet`.
    #[allow(clippy::too_many_arguments)]
    fn transform_grouping_set(
        &mut self,
        flat: &mut Vec<SortGroupClause>,
        gset: &Flat,
        tlist: &mut Vec<TargetEntry>,
        sort_clause: &[SortGroupClause],
        kind: ExprKind,
        sql99: bool,
        _toplevel: bool,
    ) -> R<GroupingSet> {
        let Flat::Set {
            kind: setkind,
            content: items,
            location,
        } = gset
        else {
            return unsupported("a grouping set");
        };
        let mut content = Vec::new();
        for n in items {
            match n {
                Flat::List(l) => {
                    // transformGroupClauseList
                    let mut seen: Vec<u32> = Vec::new();
                    let mut refs = Vec::new();
                    for item in l {
                        let Flat::Expr(e) = item else {
                            return unsupported("a grouping set inside a row");
                        };
                        let r = self.transform_group_clause_expr(
                            flat,
                            Some(&seen),
                            e,
                            tlist,
                            sort_clause,
                            kind,
                            sql99,
                            false,
                        )?;
                        if r > 0 {
                            seen.push(r);
                            refs.push(GroupingSetContent::Ref(r as i32));
                        }
                    }
                    content.push(GroupingSetContent::Set(GroupingSet {
                        kind: GROUPING_SET_SIMPLE,
                        content: refs,
                        location: flat_location(n),
                    }));
                }
                Flat::Set { .. } => content.push(GroupingSetContent::Set(
                    self.transform_grouping_set(flat, n, tlist, sort_clause, kind, sql99, false)?,
                )),
                Flat::Expr(e) => {
                    let r = self.transform_group_clause_expr(
                        flat,
                        None,
                        e,
                        tlist,
                        sort_clause,
                        kind,
                        sql99,
                        false,
                    )?;
                    content.push(GroupingSetContent::Set(GroupingSet {
                        kind: GROUPING_SET_SIMPLE,
                        content: vec![GroupingSetContent::Ref(r as i32)],
                        location: raw_location(e),
                    }));
                }
            }
        }
        if *setkind == GROUPING_SET_CUBE && content.len() > 12 {
            return Err(AnalysisError::new("CUBE is limited to 12 elements".into()));
        }
        Ok(GroupingSet {
            kind: *setkind,
            content,
            location: *location,
        })
    }

    /// `transformGroupingFunc`.
    pub fn transform_grouping_func(&mut self, g: &pb::GroupingFunc) -> R<Node> {
        if g.args.len() > 31 {
            return Err(AnalysisError::new(
                "GROUPING must have fewer than 32 arguments".into(),
            ));
        }
        let kind = self.psr().expr_kind;
        let mut args = Vec::new();
        for a in &g.args {
            args.push(self.transform_expr(a, kind)?);
        }
        let mut f = GroupingFunc {
            args,
            refs: Vec::new(),
            cols: Vec::new(),
            agglevelsup: 0,
            location: g.location,
        };
        // check_agglevels_and_constraints
        let (mut min_var, mut min_agg) = (None, None);
        for a in &f.args {
            super::nodefuncs::agg_levels(a, 0, &mut min_var, &mut min_agg);
        }
        let level = match (min_var, min_agg) {
            (None, None) => 0,
            (Some(v), None) => v,
            (None, Some(a)) => a,
            (Some(v), Some(a)) => v.min(a),
        };
        if Some(level) == min_agg {
            return pg_error("42803", "aggregate function calls cannot be nested");
        }
        if level as usize >= self.depth() {
            return unsupported("a GROUPING's level");
        }
        f.agglevelsup = level;
        match self.level_ref(level as usize).expr_kind {
            ExprKind::JoinOn
            | ExprKind::JoinUsing
            | ExprKind::FromSubselect
            | ExprKind::FromFunction
            | ExprKind::Where
            | ExprKind::GroupBy
            | ExprKind::Limit
            | ExprKind::Offset => {
                return Err(AnalysisError::new(
                    "grouping operations are not allowed here".into(),
                ));
            }
            _ => {}
        }
        self.level(level as usize).has_aggs = true;
        Ok(Node::GroupingFunc(Box::new(f)))
    }

    /// `parseCheckAggregates`, for what it writes into the query: grouping
    /// sets that expand to one set fold into a plain GROUP BY, `GROUPING()`
    /// learns its sortgroup references, and a column accepted through its
    /// table's primary key records that constraint in `constraintDeps`.
    pub fn parse_check_aggregates(&self, qry: &mut Query) -> R<()> {
        let mut gset_common: Vec<i32> = Vec::new();
        if !qry.grouping_sets.is_empty() {
            let gsets = expand_grouping_sets(&qry.grouping_sets, qry.group_distinct, 4096);
            if gsets.is_empty() {
                return Err(AnalysisError::new(
                    "too many grouping sets present (maximum 4096)".into(),
                ));
            }
            gset_common = gsets[0].clone();
            for g in &gsets[1..] {
                gset_common.retain(|x| g.contains(x));
                if gset_common.is_empty() {
                    break;
                }
            }
            if gsets.len() == 1 && !qry.group_clause.is_empty() {
                qry.grouping_sets.clear();
            }
        }
        let has_join_rtes = qry
            .rtable
            .iter()
            .any(|r| matches!(r.kind, RteKind::Join { .. }));
        let rtable = qry.rtable.clone();
        let mut group_clauses: Vec<TargetEntry> = qry
            .group_clause
            .iter()
            .filter_map(|g| {
                qry.target_list
                    .iter()
                    .find(|t| t.ressortgroupref == g.tle_sort_group_ref)
                    .cloned()
            })
            .collect();
        if has_join_rtes {
            for t in group_clauses.iter_mut() {
                t.expr = flatten_join_alias_vars(&rtable, &t.expr, 0)?;
            }
        }
        let have_non_var = group_clauses
            .iter()
            .any(|t| !matches!(t.expr, Node::Var(_)));
        let common_vars: Vec<Node> = group_clauses
            .iter()
            .filter(|t| {
                matches!(t.expr, Node::Var(_))
                    && (qry.grouping_sets.is_empty()
                        || gset_common.contains(&(t.ressortgroupref as i32)))
            })
            .map(|t| t.expr.clone())
            .collect();
        let ctx = Ctx {
            rtable: &rtable,
            group_clauses: &group_clauses,
            common_vars: &common_vars,
            have_non_var,
            has_join_rtes,
        };
        let mut func_grouped: Vec<i32> = Vec::new();
        let mut deps: Vec<Oid> = Vec::new();
        for t in qry.target_list.iter_mut() {
            ctx.finalize(&mut t.expr, 0)?;
        }
        for t in &qry.target_list {
            let e = if has_join_rtes {
                flatten_join_alias_vars(&rtable, &t.expr, 0)?
            } else {
                t.expr.clone()
            };
            self.check_ungrouped(&ctx, &e, 0, &mut func_grouped, &mut deps)?;
        }
        if let Some(h) = qry.having_qual.as_mut() {
            ctx.finalize(h, 0)?;
        }
        if let Some(h) = &qry.having_qual {
            let e = if has_join_rtes {
                flatten_join_alias_vars(&rtable, h, 0)?
            } else {
                h.clone()
            };
            self.check_ungrouped(&ctx, &e, 0, &mut func_grouped, &mut deps)?;
        }
        qry.constraint_deps.extend(deps);
        // Per spec, aggregates can't appear in a recursive term.
        let self_ref = rtable.iter().any(|r| {
            matches!(
                r.kind,
                RteKind::Cte {
                    self_reference: true,
                    ..
                }
            )
        });
        if qry.has_aggs && self_ref {
            return pg_error(
                "42P19",
                "aggregate functions are not allowed in a recursive query's recursive term",
            );
        }
        Ok(())
    }

    /// `check_ungrouped_columns_walker`.
    fn check_ungrouped(
        &self,
        ctx: &Ctx<'_>,
        n: &Node,
        sublevels: u32,
        func_grouped: &mut Vec<i32>,
        deps: &mut Vec<Oid>,
    ) -> R<()> {
        match n {
            Node::Const(_) | Node::Param(_) => return Ok(()),
            Node::Aggref(a) => {
                if a.agglevelsup == sublevels {
                    for d in &a.aggdirectargs {
                        self.check_ungrouped(ctx, d, sublevels, func_grouped, deps)?;
                    }
                    return Ok(());
                }
                if a.agglevelsup > sublevels {
                    return Ok(());
                }
            }
            Node::GroupingFunc(g) if g.agglevelsup >= sublevels => return Ok(()),
            _ => {}
        }
        if ctx.have_non_var && sublevels == 0 && ctx.group_clauses.iter().any(|t| equal(n, &t.expr))
        {
            return Ok(());
        }
        if let Node::Var(v) = n {
            if v.varlevelsup != sublevels {
                return Ok(());
            }
            if (!ctx.have_non_var || sublevels != 0)
                && ctx.group_clauses.iter().any(|t| {
                    matches!(&t.expr, Node::Var(g)
                        if g.varno == v.varno && g.varattno == v.varattno && g.varlevelsup == 0)
                })
            {
                return Ok(());
            }
            if func_grouped.contains(&v.varno) {
                return Ok(());
            }
            let rte = ctx
                .rtable
                .get((v.varno - 1) as usize)
                .ok_or_else(|| AnalysisError::new("a Var's range table entry".into()))?;
            if let RteKind::Relation { relid, .. } = &rte.kind {
                if let Some(con) = self.check_functional_grouping(*relid, v.varno, ctx.common_vars)
                {
                    func_grouped.push(v.varno);
                    deps.push(con);
                    return Ok(());
                }
            }
            let att = rte
                .eref
                .colnames
                .get((v.varattno - 1).max(0) as usize)
                .cloned()
                .unwrap_or_default();
            return Err(AnalysisError::new(if sublevels == 0 {
                format!(
                    "column \"{}.{att}\" must appear in the GROUP BY clause or be used in an aggregate function",
                    rte.eref.aliasname
                )
            } else {
                format!(
                    "subquery uses ungrouped column \"{}.{att}\" from outer query",
                    rte.eref.aliasname
                )
            }));
        }
        if let Node::SubLink(s) = n {
            if let Some(t) = &s.testexpr {
                self.check_ungrouped(ctx, t, sublevels, func_grouped, deps)?;
            }
            let mut err = Ok(());
            super::nodefuncs::walk_query_exprs(&s.subselect, &mut |e| {
                if err.is_ok() {
                    err = self.check_ungrouped(ctx, e, sublevels + 1, func_grouped, deps);
                }
            });
            return err;
        }
        for c in children(n) {
            self.check_ungrouped(ctx, c, sublevels, func_grouped, deps)?;
        }
        Ok(())
    }

    /// `check_functional_grouping`: the primary key of `relid` when every
    /// one of its columns is grouped by, as a Var of `varno`.
    fn check_functional_grouping(&self, relid: Oid, varno: i32, grouping: &[Node]) -> Option<Oid> {
        // get_primary_key_attnos(relid, deferrableOk = false)
        let (con, cols) = self.cat.primary_key(relid)?;
        let all = cols.iter().all(|attno| {
            grouping.iter().any(|g| {
                matches!(g, Node::Var(v)
                    if v.varno == varno && v.varlevelsup == 0 && v.varattno == i32::from(*attno))
            })
        });
        all.then_some(con)
    }
}

/// What the grouping checks compare against.
pub(crate) struct Ctx<'c> {
    rtable: &'c [RangeTblEntry],
    group_clauses: &'c [TargetEntry],
    common_vars: &'c [Node],
    have_non_var: bool,
    has_join_rtes: bool,
}

impl Ctx<'_> {
    /// `finalize_grouping_exprs_walker`: each GROUPING() of the query level
    /// learns the sortgroup references of its arguments.
    fn finalize(&self, n: &mut Node, sublevels: u32) -> R<()> {
        match n {
            Node::Const(_) | Node::Param(_) => return Ok(()),
            Node::Aggref(a) => {
                if a.agglevelsup == sublevels {
                    for d in a.aggdirectargs.iter_mut() {
                        self.finalize(d, sublevels)?;
                    }
                    return Ok(());
                }
                if a.agglevelsup > sublevels {
                    return Ok(());
                }
            }
            Node::GroupingFunc(g) => {
                if g.agglevelsup == sublevels {
                    let mut refs = Vec::new();
                    for arg in &g.args {
                        let expr = if self.has_join_rtes {
                            flatten_join_alias_vars(self.rtable, arg, sublevels)?
                        } else {
                            arg.clone()
                        };
                        let mut r = 0;
                        if let Node::Var(v) = &expr {
                            if v.varlevelsup == sublevels {
                                if let Some(t) = self.group_clauses.iter().find(|t| {
                                    matches!(&t.expr, Node::Var(gv)
                                        if gv.varno == v.varno && gv.varattno == v.varattno
                                            && gv.varlevelsup == 0)
                                }) {
                                    r = t.ressortgroupref;
                                }
                            }
                        } else if self.have_non_var && sublevels == 0 {
                            if let Some(t) =
                                self.group_clauses.iter().find(|t| equal(&expr, &t.expr))
                            {
                                r = t.ressortgroupref;
                            }
                        }
                        if r == 0 {
                            return Err(AnalysisError::new(
                                "arguments to GROUPING must be grouping expressions of the associated query level"
                                    .into(),
                            ));
                        }
                        refs.push(r as i32);
                    }
                    g.refs = refs;
                }
                if g.agglevelsup > sublevels {
                    return Ok(());
                }
            }
            Node::SubLink(s) => {
                if let Some(t) = s.testexpr.as_mut() {
                    self.finalize(t, sublevels)?;
                }
                return self.finalize_query(&mut s.subselect, sublevels + 1);
            }
            _ => {}
        }
        for c in super::nodefuncs::children_mut(n) {
            self.finalize(c, sublevels)?;
        }
        Ok(())
    }

    fn finalize_query(&self, q: &mut Query, sublevels: u32) -> R<()> {
        for t in q.target_list.iter_mut() {
            self.finalize(&mut t.expr, sublevels)?;
        }
        if let Some(j) = q.jointree.as_mut() {
            if let Some(w) = j.quals.as_mut() {
                self.finalize(w, sublevels)?;
            }
        }
        if let Some(h) = q.having_qual.as_mut() {
            self.finalize(h, sublevels)?;
        }
        Ok(())
    }
}

/// `flatten_join_alias_vars` (parse-time form): a Var of a join's output
/// is the join's alias expression for that column.
pub(crate) fn flatten_join_alias_vars(
    rtable: &[RangeTblEntry],
    n: &Node,
    sublevels: u32,
) -> R<Node> {
    if let Node::Var(v) = n {
        if v.varlevelsup == sublevels {
            if let Some(RangeTblEntry {
                kind: RteKind::Join { joinaliasvars, .. },
                ..
            }) = rtable.get((v.varno - 1) as usize)
            {
                if v.varattno == 0 {
                    return unsupported("a whole-row reference to a join in a grouped query");
                }
                let alias = joinaliasvars
                    .get((v.varattno - 1) as usize)
                    .ok_or_else(|| AnalysisError::new("a join alias variable".into()))?;
                let mut out = flatten_join_alias_vars(rtable, alias, 0)?;
                if sublevels > 0 {
                    increment_var_sublevels_up(&mut out, sublevels);
                }
                return Ok(out);
            }
        }
        return Ok(n.clone());
    }
    if let Node::SubLink(_) = n {
        // Grouping comparisons look only at this level's expressions.
        return Ok(n.clone());
    }
    let mut out = n.clone();
    for c in super::nodefuncs::children_mut(&mut out) {
        *c = flatten_join_alias_vars(rtable, c, sublevels)?;
    }
    Ok(out)
}

fn increment_var_sublevels_up(n: &mut Node, by: u32) {
    if let Node::Var(v) = n {
        v.varlevelsup += by;
        return;
    }
    for c in super::nodefuncs::children_mut(n) {
        increment_var_sublevels_up(c, by);
    }
}
