//! `analyze.c`: the statement-level transforms, the parse state, and the
//! entry points a view and a rule are analysed through (`view.c`'s
//! `DefineView`, `parse_utilcmd.c`'s `transformRuleStmt`).

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_type, expr_typmod};
use super::nodes::*;
use super::{unsupported, AnalysisError, RuleTrees, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

/// `ParseExprKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExprKind {
    None,
    Other,
    JoinOn,
    JoinUsing,
    FromSubselect,
    FromFunction,
    Where,
    Having,
    Filter,
    WindowPartition,
    WindowOrder,
    WindowFrameRange,
    WindowFrameRows,
    WindowFrameGroups,
    SelectTarget,
    InsertTarget,
    UpdateSource,
    UpdateTarget,
    GroupBy,
    OrderBy,
    DistinctOn,
    Limit,
    Offset,
    Returning,
    IndexExpression,
    IndexPredicate,
    Values,
    ValuesSingle,
}

/// `ParseNamespaceColumn`.
#[derive(Clone, Debug, Default)]
pub(crate) struct NsColumn {
    /// 0 for a dropped column.
    pub varno: i32,
    pub varattno: i32,
    pub vartype: Oid,
    pub vartypmod: i32,
    pub varcollid: Oid,
    pub varnosyn: i32,
    pub varattnosyn: i32,
    pub dontexpand: bool,
}

/// `ParseNamespaceItem`.
#[derive(Clone, Debug)]
pub(crate) struct NsItem {
    pub names: Alias,
    /// `p_names == p_rte->eref` (false for a JOIN USING alias).
    pub is_eref: bool,
    pub rtindex: i32,
    pub columns: Vec<NsColumn>,
    pub rel_visible: bool,
    pub cols_visible: bool,
    pub lateral_only: bool,
    pub lateral_ok: bool,
}

/// `ParseState`.
pub(crate) struct ParseState {
    pub rtable: Vec<RangeTblEntry>,
    /// Each join RTE's inputs, by rtindex (for whole-row references).
    pub joinexprs: Vec<Option<(JoinTreeNode, JoinTreeNode)>>,
    pub joinlist: Vec<JoinTreeNode>,
    pub namespace: Vec<NsItem>,
    pub lateral_active: bool,
    pub ctenamespace: Vec<CommonTableExpr>,
    pub expr_kind: ExprKind,
    pub next_resno: i32,
    pub locking_clause: Vec<pb::Node>,
    pub locked_from_parent: bool,
    pub resolve_unknowns: bool,
    pub windowdefs: Vec<pb::WindowDef>,
    pub has_aggs: bool,
    pub has_window_funcs: bool,
    pub has_target_srfs: bool,
    pub has_sub_links: bool,
    pub has_modifying_cte: bool,
    /// The INSERT/UPDATE/DELETE target's rtindex.
    pub target_rtindex: Option<i32>,
    pub is_insert: bool,
    pub parent_cte_recursive: bool,
    /// `p_parent_cte`: the CTE whose query this level is, by its position
    /// in the parent level's `ctenamespace`.
    pub parent_cte: Option<usize>,
}

impl ParseState {
    pub fn new() -> Self {
        ParseState {
            rtable: Vec::new(),
            joinexprs: Vec::new(),
            joinlist: Vec::new(),
            namespace: Vec::new(),
            lateral_active: false,
            ctenamespace: Vec::new(),
            expr_kind: ExprKind::None,
            next_resno: 1,
            locking_clause: Vec::new(),
            locked_from_parent: false,
            resolve_unknowns: true,
            windowdefs: Vec::new(),
            has_aggs: false,
            has_window_funcs: false,
            has_target_srfs: false,
            has_sub_links: false,
            has_modifying_cte: false,
            target_rtindex: None,
            is_insert: false,
            parent_cte_recursive: false,
            parent_cte: None,
        }
    }
}

/// What an analysis reads: the catalog and the search path.
pub struct AnalysisContext<'a> {
    pub catalog: &'a Catalog,
    /// The namespaces of `search_path`, as written (pg_catalog is searched
    /// first when absent, as PostgreSQL's `recomputeNamespacePath` does).
    pub search_path: Vec<Oid>,
}

pub(crate) struct Analyzer<'a> {
    pub cat: &'a Catalog,
    /// `activeSearchPath`.
    pub path: Vec<Oid>,
    pub levels: Vec<ParseState>,
}

impl<'a> Analyzer<'a> {
    pub fn new(ctx: &AnalysisContext<'a>) -> Self {
        let mut path = Vec::new();
        if !ctx.search_path.contains(&PG_CATALOG_NAMESPACE) {
            path.push(PG_CATALOG_NAMESPACE);
        }
        for n in &ctx.search_path {
            if !path.contains(n) {
                path.push(*n);
            }
        }
        Analyzer {
            cat: ctx.catalog,
            path,
            levels: Vec::new(),
        }
    }

    pub fn ps(&mut self) -> &mut ParseState {
        self.levels.last_mut().expect("a parse state")
    }

    pub fn psr(&self) -> &ParseState {
        self.levels.last().expect("a parse state")
    }

    pub fn level(&mut self, up: usize) -> &mut ParseState {
        let n = self.levels.len();
        &mut self.levels[n - 1 - up]
    }

    pub fn level_ref(&self, up: usize) -> &ParseState {
        &self.levels[self.levels.len() - 1 - up]
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// `parse_sub_analyze`.
    pub fn sub_analyze(
        &mut self,
        stmt: &pb::Node,
        locked_from_parent: bool,
        resolve_unknowns: bool,
    ) -> R<Query> {
        let mut ps = ParseState::new();
        ps.locked_from_parent = locked_from_parent;
        ps.resolve_unknowns = resolve_unknowns;
        self.levels.push(ps);
        let out = self.transform_stmt(stmt);
        self.levels.pop();
        out
    }

    /// `transformStmt`.
    pub fn transform_stmt(&mut self, stmt: &pb::Node) -> R<Query> {
        let mut q = match stmt.node.as_ref() {
            Some(N::SelectStmt(s)) => {
                if !s.values_lists.is_empty() {
                    self.transform_values_clause(s)?
                } else if s.op == pb::SetOperation::SetopNone as i32 {
                    self.transform_select_stmt(s)?
                } else {
                    self.transform_set_operation_stmt(s)?
                }
            }
            Some(N::InsertStmt(s)) => self.transform_insert_stmt(s)?,
            Some(N::UpdateStmt(s)) => self.transform_update_stmt(s)?,
            Some(N::DeleteStmt(s)) => self.transform_delete_stmt(s)?,
            Some(N::NotifyStmt(n)) => Query {
                command_type: CMD_UTILITY,
                utility_stmt: Some(UtilityStmt::Notify {
                    conditionname: n.conditionname.clone(),
                    payload: (!n.payload.is_empty()).then(|| n.payload.clone()),
                }),
                ..Default::default()
            },
            _ => return unsupported("a statement of this kind in a rule"),
        };
        q.query_source = 0;
        q.can_set_tag = true;
        Ok(q)
    }

    /// `transformSelectStmt`.
    fn transform_select_stmt(&mut self, stmt: &pb::SelectStmt) -> R<Query> {
        let mut qry = Query {
            command_type: CMD_SELECT,
            ..Default::default()
        };
        if let Some(w) = &stmt.with_clause {
            qry.has_recursive = w.recursive;
            self.transform_with_clause(w)?;
            qry.has_modifying_cte = self.ps().has_modifying_cte;
        }
        if stmt.into_clause.is_some() {
            return unsupported("SELECT ... INTO here");
        }
        self.ps().locking_clause = stmt.locking_clause.clone();
        self.ps().windowdefs = stmt
            .window_clause
            .iter()
            .filter_map(|w| match w.node.as_ref() {
                Some(N::WindowDef(d)) => Some((**d).clone()),
                _ => None,
            })
            .collect();
        self.transform_from_clause(&stmt.from_clause)?;
        qry.target_list = self.transform_target_list(&stmt.target_list, ExprKind::SelectTarget)?;
        self.mark_target_list_origins(&mut qry.target_list);
        let qual = self.transform_where_clause(stmt.where_clause.as_deref(), ExprKind::Where)?;
        qry.having_qual =
            self.transform_where_clause(stmt.having_clause.as_deref(), ExprKind::Having)?;
        qry.sort_clause = self.transform_sort_clause(
            &stmt.sort_clause,
            &mut qry.target_list,
            ExprKind::OrderBy,
            false,
        )?;
        let (group, gsets) = self.transform_group_clause(
            &stmt.group_clause,
            true,
            &mut qry.target_list,
            &qry.sort_clause,
            ExprKind::GroupBy,
            false,
        )?;
        qry.group_clause = group;
        qry.grouping_sets = gsets;
        qry.group_distinct = stmt.group_distinct;
        if stmt.distinct_clause.is_empty() {
            qry.distinct_clause = Vec::new();
            qry.has_distinct_on = false;
        } else if stmt.distinct_clause.len() == 1 && stmt.distinct_clause[0].node.is_none() {
            qry.distinct_clause =
                self.transform_distinct_clause(&mut qry.target_list, &qry.sort_clause, false)?;
            qry.has_distinct_on = false;
        } else {
            qry.distinct_clause = self.transform_distinct_on_clause(
                &stmt.distinct_clause,
                &mut qry.target_list,
                &qry.sort_clause,
            )?;
            qry.has_distinct_on = true;
        }
        qry.limit_offset =
            self.transform_limit_clause(stmt.limit_offset.as_deref(), ExprKind::Offset)?;
        qry.limit_count =
            self.transform_limit_clause(stmt.limit_count.as_deref(), ExprKind::Limit)?;
        qry.limit_option = limit_option15(stmt.limit_option);
        let windefs = std::mem::take(&mut self.ps().windowdefs);
        qry.window_clause = self.transform_window_definitions(&windefs, &mut qry.target_list)?;
        self.ps().windowdefs = windefs;
        if self.ps().resolve_unknowns {
            self.resolve_target_list_unknowns(&mut qry.target_list)?;
        }
        if !stmt.locking_clause.is_empty() {
            return unsupported("FOR UPDATE/SHARE in a stored rule");
        }
        let ps = self.ps();
        qry.rtable = std::mem::take(&mut ps.rtable);
        qry.jointree = Some(FromExpr {
            fromlist: std::mem::take(&mut ps.joinlist),
            quals: qual,
        });
        qry.has_sub_links = ps.has_sub_links;
        qry.has_window_funcs = ps.has_window_funcs;
        qry.has_target_srfs = ps.has_target_srfs;
        qry.has_aggs = ps.has_aggs;
        qry.cte_list = std::mem::take(&mut ps.ctenamespace);
        self.assign_query_collations(&mut qry)?;
        // After the collations, for reliable comparison of expressions.
        if qry.has_aggs
            || !qry.group_clause.is_empty()
            || !qry.grouping_sets.is_empty()
            || qry.having_qual.is_some()
        {
            self.parse_check_aggregates(&mut qry)?;
        }
        Ok(qry)
    }

    /// `transformValuesClause`.
    fn transform_values_clause(&mut self, stmt: &pb::SelectStmt) -> R<Query> {
        let mut qry = Query {
            command_type: CMD_SELECT,
            ..Default::default()
        };
        if let Some(w) = &stmt.with_clause {
            qry.has_recursive = w.recursive;
            self.transform_with_clause(w)?;
            qry.has_modifying_cte = self.ps().has_modifying_cte;
        }
        let mut colexprs: Vec<Vec<Node>> = Vec::new();
        let mut nrows = 0;
        for row in &stmt.values_lists {
            let items: &[pb::Node] = match row.node.as_ref() {
                Some(N::List(l)) => &l.items,
                _ => return unsupported("a VALUES row"),
            };
            let sub = self.transform_expression_list(items, ExprKind::Values, false)?;
            if nrows == 0 {
                colexprs = vec![Vec::new(); sub.len()];
            } else if sub.len() != colexprs.len() {
                return unsupported("VALUES lists must all be the same length");
            }
            for (i, e) in sub.into_iter().enumerate() {
                colexprs[i].push(e);
            }
            nrows += 1;
        }
        let mut coltypes = Vec::new();
        let mut coltypmods = Vec::new();
        let mut colcolls = Vec::new();
        for col in colexprs.iter_mut() {
            let t = self.select_common_type(col, Some("VALUES"))?;
            let taken = std::mem::take(col);
            for e in taken {
                col.push(self.coerce_to_common_type(e, t, "VALUES")?);
            }
            coltypes.push(t);
            coltypmods.push(self.select_common_typmod(col, t));
            colcolls.push(self.select_common_collation(col, true)?);
        }
        let mut rows: Vec<Vec<Node>> = vec![Vec::new(); nrows];
        for col in colexprs {
            for (r, e) in col.into_iter().enumerate() {
                rows[r].push(e);
            }
        }
        let lateral = !self.ps().rtable.is_empty()
            && rows
                .iter()
                .flatten()
                .any(|e| super::nodefuncs::contain_vars_of_level(e, 0));
        let ns =
            self.add_rte_for_values(rows, coltypes, coltypmods, colcolls, None, lateral, true)?;
        self.add_nsitem_to_query(ns, true, true, true);
        let idx = self.psr().namespace.len() - 1;
        qry.target_list = self.expand_nsitem_attrs_at(idx, 0, true, -1)?;
        qry.sort_clause = self.transform_sort_clause(
            &stmt.sort_clause,
            &mut qry.target_list,
            ExprKind::OrderBy,
            false,
        )?;
        qry.limit_offset =
            self.transform_limit_clause(stmt.limit_offset.as_deref(), ExprKind::Offset)?;
        qry.limit_count =
            self.transform_limit_clause(stmt.limit_count.as_deref(), ExprKind::Limit)?;
        qry.limit_option = limit_option15(stmt.limit_option);
        if !stmt.locking_clause.is_empty() {
            return unsupported("FOR UPDATE cannot be applied to VALUES");
        }
        let ps = self.ps();
        qry.rtable = std::mem::take(&mut ps.rtable);
        qry.jointree = Some(FromExpr {
            fromlist: std::mem::take(&mut ps.joinlist),
            quals: None,
        });
        qry.has_sub_links = ps.has_sub_links;
        qry.cte_list = std::mem::take(&mut ps.ctenamespace);
        self.assign_query_collations(&mut qry)?;
        Ok(qry)
    }

    /// `transformSetOperationStmt`.
    fn transform_set_operation_stmt(&mut self, stmt: &pb::SelectStmt) -> R<Query> {
        let mut qry = Query {
            command_type: CMD_SELECT,
            ..Default::default()
        };
        let mut leftmost = stmt.larg.as_deref();
        while let Some(l) = leftmost {
            if l.op == pb::SetOperation::SetopNone as i32 {
                break;
            }
            leftmost = l.larg.as_deref();
        }
        if leftmost.is_some_and(|l| l.into_clause.is_some()) {
            return unsupported("SELECT ... INTO is not allowed here");
        }
        if !stmt.locking_clause.is_empty() {
            return unsupported("FOR UPDATE with UNION/INTERSECT/EXCEPT");
        }
        let mut inner = stmt.clone();
        let sort = std::mem::take(&mut inner.sort_clause);
        let limit_offset = inner.limit_offset.take();
        let limit_count = inner.limit_count.take();
        let with = inner.with_clause.take();
        if let Some(w) = &with {
            qry.has_recursive = w.recursive;
            self.transform_with_clause(w)?;
            qry.has_modifying_cte = self.ps().has_modifying_cte;
        }
        let (sostmt, _) = self.transform_set_operation_tree(&inner, true, false)?;
        let SetOpNode::SetOp(sostmt) = sostmt else {
            return unsupported("a set operation tree");
        };
        let mut node = &sostmt.larg;
        let leftmost_rti = loop {
            match node {
                SetOpNode::SetOp(s) => node = &s.larg,
                SetOpNode::RangeTblRef(i) => break *i,
            }
        };
        let left_tlist: Vec<TargetEntry> =
            match &self.psr().rtable[(leftmost_rti - 1) as usize].kind {
                RteKind::Subquery { subquery, .. } => subquery.target_list.clone(),
                _ => return unsupported("a set operation leaf"),
            };
        let mut targetvars = Vec::new();
        let mut targetnames = Vec::new();
        let mut sortcols = Vec::new();
        for (i, lefttle) in left_tlist.iter().enumerate() {
            if i >= sostmt.col_types.len() {
                break;
            }
            let var = Var {
                varno: leftmost_rti,
                varattno: lefttle.resno,
                vartype: sostmt.col_types[i],
                vartypmod: sostmt.col_typmods[i],
                varcollid: sostmt.col_collations[i],
                varlevelsup: 0,
                varnosyn: leftmost_rti as u32,
                varattnosyn: lefttle.resno,
                location: super::nodefuncs::expr_location(&lefttle.expr),
            };
            let resno = self.ps().next_resno;
            self.ps().next_resno += 1;
            qry.target_list.push(TargetEntry {
                expr: Node::Var(var.clone()),
                resno,
                resname: lefttle.resname.clone(),
                ressortgroupref: 0,
                resorigtbl: 0,
                resorigcol: 0,
                resjunk: false,
            });
            targetnames.push(lefttle.resname.clone().unwrap_or_default());
            sortcols.push(NsColumn {
                varno: leftmost_rti,
                varattno: lefttle.resno,
                vartype: var.vartype,
                vartypmod: var.vartypmod,
                varcollid: var.varcollid,
                varnosyn: leftmost_rti,
                varattnosyn: lefttle.resno,
                dontexpand: false,
            });
            targetvars.push(Node::Var(var));
        }
        let sv_rtable_len = self.psr().rtable.len();
        let ns = self.add_rte_for_join(
            targetnames,
            sortcols,
            0,
            0,
            targetvars,
            Vec::new(),
            Vec::new(),
            None,
            None,
            false,
        );
        let sv_namespace = std::mem::take(&mut self.ps().namespace);
        self.add_nsitem_to_query(ns, false, false, true);
        let tllen = qry.target_list.len();
        qry.sort_clause =
            self.transform_sort_clause(&sort, &mut qry.target_list, ExprKind::OrderBy, false)?;
        self.ps().namespace = sv_namespace;
        self.ps().rtable.truncate(sv_rtable_len);
        if tllen != qry.target_list.len() {
            return unsupported("invalid UNION/INTERSECT/EXCEPT ORDER BY clause");
        }
        qry.limit_offset =
            self.transform_limit_clause(limit_offset.as_deref(), ExprKind::Offset)?;
        qry.limit_count = self.transform_limit_clause(limit_count.as_deref(), ExprKind::Limit)?;
        qry.limit_option = limit_option15(stmt.limit_option);
        qry.set_operations = Some(*sostmt);
        let ps = self.ps();
        qry.rtable = std::mem::take(&mut ps.rtable);
        qry.jointree = Some(FromExpr {
            fromlist: std::mem::take(&mut ps.joinlist),
            quals: None,
        });
        qry.has_sub_links = ps.has_sub_links;
        qry.has_window_funcs = ps.has_window_funcs;
        qry.has_target_srfs = ps.has_target_srfs;
        qry.has_aggs = ps.has_aggs;
        qry.cte_list = std::mem::take(&mut ps.ctenamespace);
        self.assign_query_collations(&mut qry)?;
        Ok(qry)
    }

    /// `transformSetOperationTree`. Returns the node and, for a caller that
    /// wants it, each output column's carrier: a leaf's own target entry
    /// (by its rtindex and position) or a synthetic `SetToDefault`.
    fn transform_set_operation_tree(
        &mut self,
        stmt: &pb::SelectStmt,
        is_top: bool,
        want_tlist: bool,
    ) -> R<(SetOpNode, Vec<SetOpCol>)> {
        if stmt.into_clause.is_some() {
            return unsupported("INTO in a set operation member");
        }
        if !stmt.locking_clause.is_empty() {
            return unsupported("FOR UPDATE with UNION/INTERSECT/EXCEPT");
        }
        let is_leaf = stmt.op == pb::SetOperation::SetopNone as i32
            || !stmt.sort_clause.is_empty()
            || stmt.limit_offset.is_some()
            || stmt.limit_count.is_some()
            || !stmt.locking_clause.is_empty()
            || stmt.with_clause.is_some();
        if is_leaf {
            let node = pb::Node {
                node: Some(N::SelectStmt(Box::new(stmt.clone()))),
            };
            let q = self.sub_analyze(&node, false, false)?;
            if !self.psr().namespace.is_empty()
                && super::nodefuncs::query_contains_vars_of_level(&q, 1)
            {
                return unsupported(
                    "UNION/INTERSECT/EXCEPT member statement cannot refer to other relations of same query level",
                );
            }
            let rtindex = self.psr().rtable.len() as i32 + 1;
            let mut cols = Vec::new();
            if want_tlist {
                for (i, t) in q.target_list.iter().enumerate() {
                    if !t.resjunk {
                        cols.push(SetOpCol::Leaf { rtindex, pos: i });
                    }
                }
            }
            let name = format!("*SELECT* {rtindex}");
            let ns = self.add_rte_for_subquery(q, Alias::new(&name), false, false)?;
            let _ = ns;
            return Ok((SetOpNode::RangeTblRef(rtindex), cols));
        }
        let context = match stmt.op {
            x if x == pb::SetOperation::SetopUnion as i32 => "UNION",
            x if x == pb::SetOperation::SetopIntersect as i32 => "INTERSECT",
            _ => "EXCEPT",
        };
        let op = stmt.op - 1;
        let recursive = self.psr().parent_cte_recursive;
        let larg = stmt
            .larg
            .as_deref()
            .ok_or_else(|| AnalysisError("a set operation without its left side".into()))?;
        let rarg = stmt
            .rarg
            .as_deref()
            .ok_or_else(|| AnalysisError("a set operation without its right side".into()))?;
        let (l, lcols) = self.transform_set_operation_tree(larg, false, true)?;
        if is_top && recursive {
            self.determine_recursive_col_types(&l, &lcols)?;
        }
        let (r, rcols) = self.transform_set_operation_tree(rarg, false, true)?;
        if lcols.len() != rcols.len() {
            return unsupported(format!(
                "each {context} query must have the same number of columns"
            ));
        }
        let mut sop = SetOperationStmt {
            op,
            all: stmt.all,
            larg: l,
            rarg: r,
            col_types: Vec::new(),
            col_typmods: Vec::new(),
            col_collations: Vec::new(),
            group_clauses: Vec::new(),
        };
        let mut out = Vec::new();
        for (lc, rc) in lcols.iter().zip(rcols.iter()) {
            let lnode = self.setop_col_expr(lc);
            let rnode = self.setop_col_expr(rc);
            let ltype = super::nodefuncs::expr_type(self.cat, &lnode);
            let rtype = super::nodefuncs::expr_type(self.cat, &rnode);
            let pair = vec![lnode.clone(), rnode.clone()];
            let (restype, best) = self.select_common_type_which(&pair, Some(context))?;
            let bestlocation = super::nodefuncs::expr_location(&pair[best]);
            let lfinal = if ltype != UNKNOWNOID {
                self.coerce_to_common_type(lnode, restype, context)?
            } else if matches!(lnode, Node::Const(_) | Node::Param(_)) {
                let n = self.coerce_to_common_type(lnode, restype, context)?;
                self.set_setop_col_expr(lc, n.clone());
                n
            } else {
                lnode
            };
            let rfinal = if rtype != UNKNOWNOID {
                self.coerce_to_common_type(rnode, restype, context)?
            } else if matches!(rnode, Node::Const(_) | Node::Param(_)) {
                let n = self.coerce_to_common_type(rnode, restype, context)?;
                self.set_setop_col_expr(rc, n.clone());
                n
            } else {
                rnode
            };
            let both = vec![lfinal, rfinal];
            let typmod = self.select_common_typmod(&both, restype);
            let union_all = op == 1 && stmt.all;
            let coll = self.select_common_collation(&both, union_all)?;
            sop.col_types.push(restype);
            sop.col_typmods.push(typmod);
            sop.col_collations.push(coll);
            if !union_all {
                let (lt, eq, _, hashable) = self.cat.sort_group_operators(restype);
                let mut hashable = hashable;
                if recursive && (restype == RECORDOID || restype == RECORDARRAYOID) {
                    hashable = true;
                }
                sop.group_clauses.push(SortGroupClause {
                    tle_sort_group_ref: 0,
                    eqop: eq,
                    sortop: lt,
                    nulls_first: false,
                    hashable,
                });
            }
            out.push(SetOpCol::Synthetic(SetToDefault {
                type_id: restype,
                type_mod: typmod,
                collation: coll,
                location: bestlocation,
            }));
        }
        Ok((SetOpNode::SetOp(Box::new(sop)), out))
    }

    fn setop_col_expr(&self, c: &SetOpCol) -> Node {
        match c {
            SetOpCol::Leaf { rtindex, pos } => {
                match &self.psr().rtable[(*rtindex - 1) as usize].kind {
                    RteKind::Subquery { subquery, .. } => subquery.target_list[*pos].expr.clone(),
                    _ => unreachable!("a set operation leaf is a subquery"),
                }
            }
            SetOpCol::Synthetic(d) => Node::SetToDefault(d.clone()),
        }
    }

    fn set_setop_col_expr(&mut self, c: &SetOpCol, n: Node) {
        if let SetOpCol::Leaf { rtindex, pos } = c {
            if let RteKind::Subquery { subquery, .. } =
                &mut self.ps().rtable[(*rtindex - 1) as usize].kind
            {
                subquery.target_list[*pos].expr = n;
            }
        }
    }

    /// `determineRecursiveColTypes`: the recursive CTE's output columns,
    /// from the non-recursive term's, named as the leftmost SELECT names
    /// them, so a self-reference in the recursive term can be resolved.
    fn determine_recursive_col_types(&mut self, larg: &SetOpNode, lcols: &[SetOpCol]) -> R<()> {
        let mut node = larg;
        let leftmost = loop {
            match node {
                SetOpNode::SetOp(s) => node = &s.larg,
                SetOpNode::RangeTblRef(i) => break *i,
            }
        };
        let names: Vec<Option<String>> = match &self.psr().rtable[(leftmost - 1) as usize].kind {
            RteKind::Subquery { subquery, .. } => subquery
                .target_list
                .iter()
                .filter(|t| !t.resjunk)
                .map(|t| t.resname.clone())
                .collect(),
            _ => return unsupported("a set operation leaf that is not a subquery"),
        };
        let tlist: Vec<TargetEntry> = lcols
            .iter()
            .zip(names)
            .enumerate()
            .map(|(i, (c, name))| TargetEntry {
                expr: self.setop_col_expr(c),
                resno: i as i32 + 1,
                resname: name,
                ressortgroupref: 0,
                resorigtbl: 0,
                resorigcol: 0,
                resjunk: false,
            })
            .collect();
        let idx = self
            .psr()
            .parent_cte
            .ok_or_else(|| AnalysisError("a recursive query without its CTE".into()))?;
        let cat = self.cat;
        let cte = self
            .level(1)
            .ctenamespace
            .get_mut(idx)
            .ok_or_else(|| AnalysisError("a recursive query's CTE".into()))?;
        analyze_cte_target_list(cat, cte, &tlist)
    }

    /// `transformWithClause`. For WITH RECURSIVE every CTE is visible to
    /// every other and to itself, so all of them join `p_ctenamespace`
    /// first, in the order `makeDependencyGraph`'s topological sort gives,
    /// and are analysed in that order; otherwise each CTE is analysed in
    /// turn and then joins the namespace, so it sees only the ones before
    /// it. SEARCH / CYCLE and data-modifying CTEs are not transcribed and
    /// refuse by name.
    fn transform_with_clause(&mut self, w: &pb::WithClause) -> R<()> {
        let ctes: Vec<&pb::CommonTableExpr> = w
            .ctes
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::CommonTableExpr(c)) => Some(c.as_ref()),
                _ => None,
            })
            .collect();
        for (i, c) in ctes.iter().enumerate() {
            if let Some(dup) = ctes[i + 1..].iter().find(|c2| c2.ctename == c.ctename) {
                return Err(AnalysisError(format!(
                    "WITH query name \"{}\" specified more than once",
                    dup.ctename
                )));
            }
            if !matches!(
                c.ctequery.as_deref().and_then(|q| q.node.as_ref()),
                Some(N::SelectStmt(_))
            ) {
                return unsupported("a data-modifying statement in WITH in a stored rule");
            }
            if c.search_clause.is_some() || c.cycle_clause.is_some() {
                return unsupported("SEARCH or CYCLE in WITH in a stored rule");
            }
        }
        let blank = |c: &pb::CommonTableExpr| CommonTableExpr {
            ctename: c.ctename.clone(),
            aliascolnames: c
                .aliascolnames
                .iter()
                .filter_map(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect(),
            // CTEMaterialize: the protobuf enum is the C enum plus one.
            ctematerialized: (c.ctematerialized - 1).max(0),
            ctequery: Box::default(),
            location: c.location,
            cterecursive: false,
            cterefcount: 0,
            ctecolnames: Vec::new(),
            ctecoltypes: Vec::new(),
            ctecoltypmods: Vec::new(),
            ctecolcollations: Vec::new(),
            analyzed: false,
        };
        if w.recursive {
            let order = cte_dependency_order(&ctes)?;
            let base = self.psr().ctenamespace.len();
            for (i, recursive) in &order {
                let mut cte = blank(ctes[*i]);
                cte.cterecursive = *recursive;
                self.ps().ctenamespace.push(cte);
            }
            for (pos, (i, recursive)) in order.iter().enumerate() {
                let stmt = ctes[*i]
                    .ctequery
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a WITH query".into()))?;
                // checkWellFormedRecursion's first test.
                if *recursive
                    && !matches!(stmt.node.as_ref(),
                        Some(N::SelectStmt(s)) if s.op == pb::SetOperation::SetopUnion as i32)
                {
                    return Err(AnalysisError(format!(
                        "recursive query \"{}\" does not have the form non-recursive-term UNION [ALL] recursive-term",
                        ctes[*i].ctename
                    )));
                }
                let query = self.sub_analyze_cte(stmt, base + pos, *recursive)?;
                let mut cte = self.psr().ctenamespace[base + pos].clone();
                finish_cte(self.cat, &mut cte, query)?;
                self.ps().ctenamespace[base + pos] = cte;
            }
        } else {
            for c in ctes {
                let stmt = c
                    .ctequery
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a WITH query".into()))?;
                let query = self.sub_analyze_cte(stmt, usize::MAX, false)?;
                let mut cte = blank(c);
                finish_cte(self.cat, &mut cte, query)?;
                self.ps().ctenamespace.push(cte);
            }
        }
        Ok(())
    }

    /// `parse_sub_analyze` of a CTE's query, `parentCTE` set.
    fn sub_analyze_cte(&mut self, stmt: &pb::Node, idx: usize, recursive: bool) -> R<Query> {
        let mut ps = ParseState::new();
        ps.parent_cte = (idx != usize::MAX).then_some(idx);
        ps.parent_cte_recursive = recursive;
        self.levels.push(ps);
        let out = self.transform_stmt(stmt);
        self.levels.pop();
        out
    }

    /// `transformInsertStmt` (VALUES and SELECT sources).
    fn transform_insert_stmt(&mut self, stmt: &pb::InsertStmt) -> R<Query> {
        super::target::transform_insert_stmt(self, stmt)
    }

    fn transform_update_stmt(&mut self, stmt: &pb::UpdateStmt) -> R<Query> {
        super::target::transform_update_stmt(self, stmt)
    }

    fn transform_delete_stmt(&mut self, stmt: &pb::DeleteStmt) -> R<Query> {
        super::target::transform_delete_stmt(self, stmt)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SetOpCol {
    Leaf { rtindex: i32, pos: usize },
    Synthetic(SetToDefault),
}

/// `analyzeCTE` after the query: CTE queries never set the command tag;
/// a non-recursive CTE takes its columns from the query, and a recursive
/// one's (already set from its non-recursive term) must agree with it.
fn finish_cte(cat: &Catalog, cte: &mut CommonTableExpr, mut query: Query) -> R<()> {
    if query.command_type != CMD_SELECT {
        return unsupported("a data-modifying statement in WITH in a stored rule");
    }
    query.can_set_tag = false;
    if !cte.cterecursive {
        analyze_cte_target_list(cat, cte, &query.target_list)?;
    } else {
        let cols: Vec<&TargetEntry> = query.target_list.iter().filter(|t| !t.resjunk).collect();
        if cols.len() != cte.ctecoltypes.len() {
            return Err(AnalysisError(
                "wrong number of output columns in WITH".into(),
            ));
        }
        for (i, t) in cols.iter().enumerate() {
            if expr_type(cat, &t.expr) != cte.ctecoltypes[i]
                || expr_typmod(cat, &t.expr) != cte.ctecoltypmods[i]
            {
                return Err(AnalysisError(format!(
                    "recursive query \"{}\" column {} has a different type in its non-recursive term",
                    cte.ctename,
                    i + 1
                )));
            }
            if expr_collation(cat, &t.expr) != cte.ctecolcollations[i] {
                return Err(AnalysisError(format!(
                    "recursive query \"{}\" column {} has a different collation in its non-recursive term",
                    cte.ctename,
                    i + 1
                )));
            }
        }
    }
    *cte.ctequery = query;
    cte.analyzed = true;
    Ok(())
}

/// `analyzeCTETargetList`.
fn analyze_cte_target_list(
    cat: &Catalog,
    cte: &mut CommonTableExpr,
    tlist: &[TargetEntry],
) -> R<()> {
    let numaliases = cte.aliascolnames.len();
    cte.ctecolnames = cte.aliascolnames.clone();
    cte.ctecoltypes.clear();
    cte.ctecoltypmods.clear();
    cte.ctecolcollations.clear();
    let mut varattno = 0;
    for te in tlist.iter().filter(|t| !t.resjunk) {
        varattno += 1;
        if varattno > numaliases {
            cte.ctecolnames.push(te.resname.clone().unwrap_or_default());
        }
        let (mut t, mut m, mut c) = (
            expr_type(cat, &te.expr),
            expr_typmod(cat, &te.expr),
            expr_collation(cat, &te.expr),
        );
        // A recursive CTE's unknown column is text.
        if cte.cterecursive && t == UNKNOWNOID {
            t = TEXTOID;
            m = -1;
            if c == 0 {
                c = DEFAULT_COLLATION_OID;
            }
        }
        cte.ctecoltypes.push(t);
        cte.ctecoltypmods.push(m);
        cte.ctecolcollations.push(c);
    }
    if varattno < numaliases {
        return Err(AnalysisError(format!(
            "WITH query \"{}\" has {varattno} columns available but {numaliases} columns specified",
            cte.ctename
        )));
    }
    Ok(())
}

/// `makeDependencyGraph` and `TopologicalSort` for WITH RECURSIVE: the
/// order to analyse the CTEs in (by their position in the clause), and
/// whether each refers to itself. A CTE name an inner WITH also defines is
/// refused rather than scoped (`WalkInnerWith` is not transcribed).
fn cte_dependency_order(ctes: &[&pb::CommonTableExpr]) -> R<Vec<(usize, bool)>> {
    let names: Vec<&str> = ctes.iter().map(|c| c.ctename.as_str()).collect();
    let mut deps: Vec<std::collections::BTreeSet<usize>> = vec![Default::default(); ctes.len()];
    let mut recursive = vec![false; ctes.len()];
    for (i, c) in ctes.iter().enumerate() {
        let Some(q) = c.ctequery.as_deref().and_then(|q| q.node.as_ref()) else {
            continue;
        };
        let mut refs = Vec::new();
        let mut inner = Vec::new();
        for (n, _, _, _) in q.nodes() {
            match n {
                pg_query::NodeRef::RangeVar(rv) if rv.schemaname.is_empty() => {
                    refs.push(rv.relname.clone())
                }
                pg_query::NodeRef::CommonTableExpr(ic) => inner.push(ic.ctename.clone()),
                _ => {}
            }
        }
        for r in refs {
            let Some(j) = names.iter().position(|n| *n == r) else {
                continue;
            };
            if inner.contains(&r) {
                return unsupported("a WITH RECURSIVE name an inner WITH also defines");
            }
            if j == i {
                recursive[i] = true;
            } else {
                deps[i].insert(j);
            }
        }
    }
    let mut items: Vec<(usize, std::collections::BTreeSet<usize>)> =
        deps.into_iter().enumerate().collect();
    for i in 0..items.len() {
        let j = (i..items.len())
            .find(|&j| items[j].1.is_empty())
            .ok_or_else(|| {
                AnalysisError("mutual recursion between WITH items is not implemented".into())
            })?;
        items.swap(i, j);
        let id = items[i].0;
        for item in items.iter_mut().skip(i + 1) {
            item.1.remove(&id);
        }
    }
    Ok(items
        .into_iter()
        .map(|(id, _)| (id, recursive[id]))
        .collect())
}

/// `OffsetVarNodes(node, offset, 0)` on a query: every Var of this level and
/// every rtindex in the jointree moved by `offset`.
fn offset_var_nodes(q: &mut Query, offset: i32) {
    super::nodefuncs::offset_query(q, offset, 0);
}

/// The stored trees of `CREATE VIEW` (`DefineView`): the analysed query,
/// with `OLD` and `NEW` placed in front of its range table as
/// `UpdateRangeTableOfViewParse` places them.
pub fn analyze_create_view(
    ctx: &AnalysisContext<'_>,
    source: &str,
    stmt_index: usize,
    view_relid: Oid,
) -> R<RuleTrees> {
    let parsed = parse_source(source)?;
    let raw = parsed
        .stmts
        .get(stmt_index)
        .ok_or_else(|| AnalysisError("no such statement".into()))?;
    let Some(N::ViewStmt(v)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE VIEW");
    };
    let query = v
        .query
        .as_deref()
        .ok_or_else(|| AnalysisError("a view without a query".into()))?;
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    let mut q = a.transform_stmt(query)?;
    a.levels.pop();
    q.stmt_location = raw.stmt_location;
    q.stmt_len = raw.stmt_len;
    if q.command_type != CMD_SELECT {
        return unsupported("a view over a non-SELECT");
    }
    // The column list renames the outputs.
    let mut aliases = v.aliases.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::String(s)) => Some(s.sval.clone()),
        _ => None,
    });
    for t in q.target_list.iter_mut().filter(|t| !t.resjunk) {
        match aliases.next() {
            Some(a) => t.resname = Some(a),
            None => break,
        }
    }
    let colnames: Vec<String> = q
        .target_list
        .iter()
        .filter(|t| !t.resjunk)
        .map(|t| t.resname.clone().unwrap_or_default())
        .collect();
    Ok(store_view_query(q, colnames, view_relid, 'v'))
}

/// The stored trees of `CREATE MATERIALIZED VIEW`: `create_ctas_internal`
/// hands the query `transformCreateTableAsStmt` analysed (with
/// `transformStmt`, so no statement location) to `StoreViewQuery`. Its
/// column list names the relation's attributes, which the OLD and NEW
/// entries carry, but does not rename the target list.
pub fn analyze_create_matview(
    ctx: &AnalysisContext<'_>,
    source: &str,
    stmt_index: usize,
    matview_relid: Oid,
) -> R<RuleTrees> {
    let parsed = parse_source(source)?;
    let raw = parsed
        .stmts
        .get(stmt_index)
        .ok_or_else(|| AnalysisError("no such statement".into()))?;
    let Some(N::CreateTableAsStmt(c)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE MATERIALIZED VIEW");
    };
    if c.objtype != pb::ObjectType::ObjectMatview as i32 {
        return unsupported("not a CREATE MATERIALIZED VIEW");
    }
    let query = c
        .query
        .as_deref()
        .ok_or_else(|| AnalysisError("a materialized view without a query".into()))?;
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    let q = a.transform_stmt(query)?;
    a.levels.pop();
    if q.command_type != CMD_SELECT {
        return unsupported("a materialized view over a non-SELECT");
    }
    if q.has_modifying_cte {
        return Err(AnalysisError(
            "materialized views must not use data-modifying statements in WITH".into(),
        ));
    }
    let mut aliases = c
        .into
        .as_ref()
        .map(|i| i.col_names.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|n| match n.node {
            Some(N::String(s)) => Some(s.sval),
            _ => None,
        });
    let colnames: Vec<String> = q
        .target_list
        .iter()
        .filter(|t| !t.resjunk)
        .map(|t| {
            aliases
                .next()
                .unwrap_or_else(|| t.resname.clone().unwrap_or_default())
        })
        .collect();
    Ok(store_view_query(q, colnames, matview_relid, 'm'))
}

/// `StoreViewQuery`: `UpdateRangeTableOfViewParse` puts OLD and NEW
/// entries for the relation in front of the range table (every Var moves
/// up two), then the query is the `_RETURN` rule's one action.
fn store_view_query(
    mut q: Query,
    colnames: Vec<String>,
    view_relid: Oid,
    relkind: char,
) -> RuleTrees {
    let rte = |name: &str| RangeTblEntry {
        alias: Some(Alias::new(name)),
        eref: Alias {
            aliasname: name.to_string(),
            colnames: colnames.clone(),
        },
        kind: RteKind::Relation {
            relid: view_relid,
            relkind,
            rellockmode: 1,
        },
        lateral: false,
        inh: false,
        in_from_cl: false,
        required_perms: 0,
        check_as_user: 0,
        selected_cols: Default::default(),
        inserted_cols: Default::default(),
        updated_cols: Default::default(),
    };
    offset_var_nodes(&mut q, 2);
    q.rtable.insert(0, rte("new"));
    q.rtable.insert(0, rte("old"));
    RuleTrees {
        ev_qual: "<>".to_string(),
        ev_action: super::out::queries_to_string(&[q]),
    }
}

/// The stored trees of `CREATE RULE` (`transformRuleStmt`).
pub fn analyze_create_rule(
    ctx: &AnalysisContext<'_>,
    source: &str,
    stmt_index: usize,
    relid: Oid,
) -> R<RuleTrees> {
    let parsed = parse_source(source)?;
    let raw = parsed
        .stmts
        .get(stmt_index)
        .ok_or_else(|| AnalysisError("no such statement".into()))?;
    let Some(N::RuleStmt(r)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE RULE");
    };
    let event = r.event - 1;
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    let old = a.add_rte_for_relation_oid(relid, Some(Alias::new("old")), 1, false, false)?;
    let new = a.add_rte_for_relation_oid(relid, Some(Alias::new("new")), 1, false, false)?;
    a.ps().rtable[0].required_perms = 0;
    a.ps().rtable[1].required_perms = 0;
    match event {
        CMD_SELECT => a.add_nsitem_to_query(old, false, true, true),
        CMD_UPDATE => {
            a.add_nsitem_to_query(old, false, true, true);
            a.add_nsitem_to_query(new, false, true, true);
        }
        CMD_INSERT => a.add_nsitem_to_query(new, false, true, true),
        CMD_DELETE => a.add_nsitem_to_query(old, false, true, true),
        _ => return unsupported("this rule event"),
    }
    let mut qual = a.transform_where_clause(r.where_clause.as_deref(), ExprKind::Where)?;
    if let Some(q) = qual.as_mut() {
        a.assign_expr_collations(q)?;
    }
    if a.psr().rtable.len() != 2 {
        return unsupported("rule WHERE condition cannot contain references to other relations");
    }
    let mut actions = Vec::new();
    if r.actions.is_empty() {
        let ps = a.ps();
        actions.push(Query {
            command_type: CMD_NOTHING,
            rtable: std::mem::take(&mut ps.rtable),
            jointree: Some(FromExpr {
                fromlist: Vec::new(),
                quals: None,
            }),
            ..Default::default()
        });
    } else {
        a.levels.pop();
        for action in &r.actions {
            let mut sub = Analyzer::new(ctx);
            sub.levels.push(ParseState::new());
            let old =
                sub.add_rte_for_relation_oid(relid, Some(Alias::new("old")), 1, false, false)?;
            let new =
                sub.add_rte_for_relation_oid(relid, Some(Alias::new("new")), 1, false, false)?;
            sub.ps().rtable[0].required_perms = 0;
            sub.ps().rtable[1].required_perms = 0;
            sub.add_nsitem_to_query(old, false, true, false);
            sub.add_nsitem_to_query(new, false, true, false);
            let mut top = sub.transform_stmt(action)?;
            if top.command_type == CMD_UTILITY && qual.is_some() {
                return unsupported(
                    "rules with WHERE conditions can only have SELECT, INSERT, UPDATE, or DELETE actions",
                );
            }
            let used = |n: i32, top: &Query| {
                super::nodefuncs::range_table_entry_used_query(get_insert_select(top), n, 0)
                    || qual
                        .as_ref()
                        .is_some_and(|q| super::nodefuncs::range_table_entry_used(q, n, 0))
            };
            let has_old = used(1, &top);
            let has_new = used(2, &top);
            match event {
                CMD_SELECT if has_old || has_new => {
                    return unsupported("ON SELECT rule cannot use OLD or NEW")
                }
                CMD_INSERT if has_old => return unsupported("ON INSERT rule cannot use OLD"),
                CMD_DELETE if has_new => return unsupported("ON DELETE rule cannot use NEW"),
                _ => {}
            }
            if has_old || (has_new && event == CMD_UPDATE) {
                let sub_qry = get_insert_select_mut(&mut top);
                if sub_qry.set_operations.is_some() {
                    return unsupported(
                        "conditional UNION/INTERSECT/EXCEPT statements are not implemented",
                    );
                }
                if let Some(j) = sub_qry.jointree.as_mut() {
                    j.fromlist.push(JoinTreeNode::RangeTblRef(1));
                }
            }
            actions.push(top);
        }
    }
    Ok(RuleTrees {
        ev_qual: super::out::node_to_string(qual.as_ref()),
        ev_action: super::out::queries_to_string(&actions),
    })
}

/// `getInsertSelectQuery`: an INSERT ... SELECT's SELECT, where OLD and NEW
/// were pushed down.
fn get_insert_select(q: &Query) -> &Query {
    if q.command_type == CMD_INSERT && q.rtable.len() >= 3 {
        if let Some(j) = &q.jointree {
            if let [JoinTreeNode::RangeTblRef(i)] = j.fromlist.as_slice() {
                if let RteKind::Subquery { subquery, .. } = &q.rtable[(*i - 1) as usize].kind {
                    if subquery.rtable.len() >= 2 {
                        return subquery;
                    }
                }
            }
        }
    }
    q
}

fn get_insert_select_mut(q: &mut Query) -> &mut Query {
    let target = if q.command_type == CMD_INSERT && q.rtable.len() >= 3 {
        match &q.jointree {
            Some(j) => match j.fromlist.as_slice() {
                [JoinTreeNode::RangeTblRef(i)] => match &q.rtable[(*i - 1) as usize].kind {
                    RteKind::Subquery { subquery, .. } if subquery.rtable.len() >= 2 => Some(*i),
                    _ => None,
                },
                _ => None,
            },
            None => None,
        }
    } else {
        None
    };
    match target {
        Some(i) => match &mut q.rtable[(i - 1) as usize].kind {
            RteKind::Subquery { subquery, .. } => subquery,
            _ => unreachable!("checked above"),
        },
        None => q,
    }
}

fn parse_source(source: &str) -> R<pb::ParseResult> {
    pg_query::parse(source)
        .map(|r| r.protobuf)
        .map_err(|e| AnalysisError(format!("parse: {e}")))
}

/// How `initdb` feeds a file to the single-user backend (`-j` mode,
/// `InteractiveBackend`): each query ends at a `;` followed by a blank line.
/// Returns each query's text.
pub fn split_initdb_file(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c == '\n' && cur.len() > 1 && cur.ends_with(";\n") {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// PostgreSQL 15's `LimitOption` for the grammar's: COUNT (0) unless WITH
/// TIES (1). A statement without a limit keeps the zeroed COUNT too, as
/// `makeNode(SelectStmt)` leaves it.
fn limit_option15(proto: i32) -> i32 {
    i32::from(proto == pb::LimitOption::WithTies as i32)
}
