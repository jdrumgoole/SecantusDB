//! `analyze.c`: the statement-level transforms, the parse state, and the
//! entry points a view and a rule are analysed through (`view.c`'s
//! `DefineView`, `parse_utilcmd.c`'s `transformRuleStmt`).

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_type, expr_typmod};
use super::nodes::*;
use super::{pg_error, unsupported, AnalysisError, RuleTrees, R};
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
    CycleMark,
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
    /// `p_multiassign_exprs`: the MULTIEXPR sublinks of `SET (a, b) =
    /// (SELECT ...)`, appended to the UPDATE's target list as junk.
    pub multiassign_exprs: Vec<TargetEntry>,
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
            multiassign_exprs: Vec::new(),
        }
    }
}

/// What an analysis reads: the catalog and the search path.
pub struct AnalysisContext<'a> {
    pub catalog: &'a Catalog,
    /// The namespaces of `search_path`, as written (pg_catalog is searched
    /// first when absent, as PostgreSQL's `recomputeNamespacePath` does).
    pub search_path: Vec<Oid>,
    /// `allowSystemTableMods`, as initdb runs: a view may have an
    /// `anyarray` column (CHKATYPE_ANYARRAY).
    pub allow_system_table_mods: bool,
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
        for lc in &stmt.locking_clause {
            if let Some(N::LockingClause(lc)) = lc.node.as_ref() {
                let rels: Vec<pb::RangeVar> = lc
                    .locked_rels
                    .iter()
                    .filter_map(|n| match n.node.as_ref() {
                        Some(N::RangeVar(rv)) => Some(rv.clone()),
                        _ => None,
                    })
                    .collect();
                transform_locking_clause(
                    &mut qry,
                    &rels,
                    lc.strength - 1,
                    lc.wait_policy - 1,
                    false,
                )?;
            }
        }
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
                return pg_error("42601", "VALUES lists must all be the same length");
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
            let strength = match stmt.locking_clause[0].node.as_ref() {
                Some(N::LockingClause(lc)) => lc.strength - 1,
                _ => LCS_FORUPDATE,
            };
            return pg_error(
                "0A000",
                format!("{} cannot be applied to VALUES", lcs_as_string(strength)),
            );
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
            let strength = match stmt.locking_clause[0].node.as_ref() {
                Some(N::LockingClause(lc)) => lc.strength - 1,
                _ => LCS_FORUPDATE,
            };
            return pg_error(
                "0A000",
                format!(
                    "{} is not allowed with UNION/INTERSECT/EXCEPT",
                    lcs_as_string(strength)
                ),
            );
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
            let strength = match stmt.locking_clause[0].node.as_ref() {
                Some(N::LockingClause(lc)) => lc.strength - 1,
                _ => LCS_FORUPDATE,
            };
            return pg_error(
                "0A000",
                format!(
                    "{} is not allowed with UNION/INTERSECT/EXCEPT",
                    lcs_as_string(strength)
                ),
            );
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
            .ok_or_else(|| AnalysisError::new("a set operation without its left side".into()))?;
        let rarg = stmt
            .rarg
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a set operation without its right side".into()))?;
        let (l, lcols) = self.transform_set_operation_tree(larg, false, true)?;
        if is_top && recursive {
            self.determine_recursive_col_types(&l, &lcols)?;
        }
        let (r, rcols) = self.transform_set_operation_tree(rarg, false, true)?;
        if lcols.len() != rcols.len() {
            return pg_error(
                "42601",
                format!("each {context} query must have the same number of columns"),
            );
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
            .ok_or_else(|| AnalysisError::new("a recursive query without its CTE".into()))?;
        let cat = self.cat;
        let cte = self
            .level(1)
            .ctenamespace
            .get_mut(idx)
            .ok_or_else(|| AnalysisError::new("a recursive query's CTE".into()))?;
        analyze_cte_target_list(cat, cte, &tlist)
    }

    /// `transformWithClause`. For WITH RECURSIVE every CTE is visible to
    /// every other and to itself, so all of them join `p_ctenamespace`
    /// first, in the order `makeDependencyGraph`'s topological sort gives,
    /// and are analysed in that order; otherwise each CTE is analysed in
    /// turn and then joins the namespace, so it sees only the ones before
    /// it. SEARCH / CYCLE and data-modifying CTEs are not transcribed and
    /// refuse by name.
    pub(crate) fn transform_with_clause(&mut self, w: &pb::WithClause) -> R<()> {
        let ctes: Vec<&pb::CommonTableExpr> = w
            .ctes
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::CommonTableExpr(c)) => Some(c.as_ref()),
                _ => None,
            })
            .collect();
        for (i, c) in ctes.iter().enumerate() {
            let q = c.ctequery.as_deref().and_then(|q| q.node.as_ref());
            if matches!(q, Some(N::MergeStmt(_))) {
                return pg_error("0A000", "MERGE not supported in WITH query");
            }
            if let Some(dup) = ctes[i + 1..].iter().find(|c2| c2.ctename == c.ctename) {
                return pg_error(
                    "42712",
                    format!(
                        "WITH query name \"{}\" specified more than once",
                        dup.ctename
                    ),
                );
            }
            if !matches!(q, Some(N::SelectStmt(_))) {
                // A data-modifying statement.
                self.ps().has_modifying_cte = true;
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
            // The raw SEARCH clause is the node's from the start: a
            // self-reference's entry already has its sequence column.
            search_clause: c.search_clause.as_ref().map(|sc| CteSearchClause {
                search_col_list: super::clause::string_list(&sc.search_col_list),
                search_breadth_first: sc.search_breadth_first,
                search_seq_column: sc.search_seq_column.clone(),
                location: sc.location,
            }),
            cycle_clause: None,
            analyzed: false,
        };
        if w.recursive {
            let order = super::cte_walk::dependency_order(&ctes)?;
            super::cte_walk::check_well_formed_recursion(&ctes, &order)?;
            let base = self.psr().ctenamespace.len();
            for (i, recursive) in &order {
                let mut cte = blank(ctes[*i]);
                cte.cterecursive = *recursive;
                self.ps().ctenamespace.push(cte);
            }
            for (pos, (i, _)) in order.iter().enumerate() {
                self.analyze_cte(ctes[*i], base + pos)?;
            }
        } else {
            for c in ctes {
                // Not visible to its own query: it joins the list once
                // analysed.
                self.analyze_cte_detached(c, blank(c))?;
            }
        }
        Ok(())
    }

    /// `analyzeCTE` for a CTE already in `p_ctenamespace` at `idx` (WITH
    /// RECURSIVE).
    fn analyze_cte(&mut self, raw: &pb::CommonTableExpr, idx: usize) -> R<()> {
        let mut cte = self.psr().ctenamespace[idx].clone();
        self.analyze_cte_cycle_mark(raw, &mut cte)?;
        self.ps().ctenamespace[idx] = cte;
        let stmt = raw
            .ctequery
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a WITH query".into()))?;
        let recursive = self.psr().ctenamespace[idx].cterecursive;
        let query = self.sub_analyze_cte(stmt, idx, recursive)?;
        let mut cte = self.psr().ctenamespace[idx].clone();
        self.finish_cte(raw, &mut cte, query)?;
        self.ps().ctenamespace[idx] = cte;
        Ok(())
    }

    /// `analyzeCTE` for a non-recursive WITH's member, which joins the
    /// namespace only once analysed.
    fn analyze_cte_detached(
        &mut self,
        raw: &pb::CommonTableExpr,
        mut cte: CommonTableExpr,
    ) -> R<()> {
        self.analyze_cte_cycle_mark(raw, &mut cte)?;
        let stmt = raw
            .ctequery
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a WITH query".into()))?;
        let query = self.sub_analyze_cte(stmt, usize::MAX, false)?;
        self.finish_cte(raw, &mut cte, query)?;
        self.ps().ctenamespace.push(cte);
        Ok(())
    }

    /// The first step of `analyzeCTE`: the CYCLE mark's values, their
    /// common type and its `<>` operator, before the query (which may
    /// refer to the mark column).
    fn analyze_cte_cycle_mark(
        &mut self,
        raw: &pb::CommonTableExpr,
        cte: &mut CommonTableExpr,
    ) -> R<()> {
        let Some(cc) = raw.cycle_clause.as_deref() else {
            return Ok(());
        };
        let value = cc
            .cycle_mark_value
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a CYCLE mark value".into()))?;
        let default = cc
            .cycle_mark_default
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a CYCLE mark default".into()))?;
        let value = self.transform_expr(value, ExprKind::CycleMark)?;
        let default = self.transform_expr(default, ExprKind::CycleMark)?;
        let pair = [value.clone(), default.clone()];
        let ty = self.select_common_type(&pair, Some("CYCLE"))?;
        let value = self.coerce_to_common_type(value, ty, "CYCLE/SET/TO")?;
        let default = self.coerce_to_common_type(default, ty, "CYCLE/SET/DEFAULT")?;
        let pair = [value.clone(), default.clone()];
        let typmod = self.select_common_typmod(&pair, ty);
        let collation = self.select_common_collation(&pair, true)?;
        let eq = self.cat.typcache(ty).eq_opr;
        if eq == 0 {
            return pg_error(
                "42883",
                format!(
                    "could not identify an equality operator for type {}",
                    self.cat.format_type_be(ty)
                ),
            );
        }
        let neop = self.cat.oper_row(eq).map_or(0, |o| o.negate);
        if neop == 0 {
            return pg_error(
                "42883",
                format!(
                    "could not identify an inequality operator for type {}",
                    self.cat.format_type_be(ty)
                ),
            );
        }
        cte.cycle_clause = Some(CteCycleClause {
            cycle_col_list: super::clause::string_list(&cc.cycle_col_list),
            cycle_mark_column: cc.cycle_mark_column.clone(),
            cycle_mark_value: value,
            cycle_mark_default: default,
            cycle_path_column: cc.cycle_path_column.clone(),
            location: cc.location,
            cycle_mark_type: ty,
            cycle_mark_typmod: typmod,
            cycle_mark_collation: collation,
            cycle_mark_neop: neop,
        });
        Ok(())
    }

    /// `analyzeCTE` after the query.
    fn finish_cte(
        &mut self,
        raw: &pb::CommonTableExpr,
        cte: &mut CommonTableExpr,
        mut query: Query,
    ) -> R<()> {
        // Data-modifying WITH only at the top level of a query.
        if query.command_type != CMD_SELECT && self.depth() > 1 {
            return pg_error(
                "0A000",
                "WITH clause containing a data-modifying statement must be at the top level",
            );
        }
        query.can_set_tag = false;
        let cat = self.cat;
        if !cte.cterecursive {
            let tl = if query.command_type == CMD_SELECT {
                query.target_list.clone()
            } else {
                query.returning_list.clone()
            };
            analyze_cte_target_list(cat, cte, &tl)?;
        } else {
            let cols: Vec<&TargetEntry> = query.target_list.iter().filter(|t| !t.resjunk).collect();
            if cols.len() != cte.ctecoltypes.len() {
                return Err(AnalysisError::new(
                    "wrong number of output columns in WITH".into(),
                ));
            }
            for (i, t) in cols.iter().enumerate() {
                let (ty, tm) = (expr_type(cat, &t.expr), expr_typmod(cat, &t.expr));
                if ty != cte.ctecoltypes[i] || tm != cte.ctecoltypmods[i] {
                    return Err(AnalysisError::pg(
                        "42804",
                        format!(
                            "recursive query \"{}\" column {} has type {} in non-recursive term but type {} overall",
                            cte.ctename,
                            i + 1,
                            cat.format_type_with_typemod(cte.ctecoltypes[i], cte.ctecoltypmods[i]),
                            cat.format_type_with_typemod(ty, tm)
                        ),
                    )
                    .with_hint("Cast the output of the non-recursive term to the correct type."));
                }
                let coll = expr_collation(cat, &t.expr);
                if coll != cte.ctecolcollations[i] {
                    return Err(AnalysisError::pg(
                        "42P21",
                        format!(
                            "recursive query \"{}\" column {} has collation \"{}\" in non-recursive term but collation \"{}\" overall",
                            cte.ctename,
                            i + 1,
                            cat.collation_name(cte.ctecolcollations[i]),
                            cat.collation_name(coll)
                        ),
                    )
                    .with_hint("Use the COLLATE clause to set the collation of the non-recursive term."));
                }
            }
        }
        *cte.ctequery = query;
        cte.analyzed = true;
        // The SEARCH and CYCLE clauses' checks.
        let search = raw.search_clause.as_ref();
        let cycle = raw.cycle_clause.as_deref();
        if search.is_some() || cycle.is_some() {
            if !cte.cterecursive {
                return pg_error("42601", "WITH query is not recursive");
            }
            if let Some(sos) = &cte.ctequery.set_operations {
                if !matches!(sos.larg, SetOpNode::RangeTblRef(_)) {
                    return pg_error(
                        "0A000",
                        "with a SEARCH or CYCLE clause, the left side of the UNION must be a SELECT",
                    );
                }
                if !matches!(sos.rarg, SetOpNode::RangeTblRef(_)) {
                    return pg_error(
                        "42601",
                        "with a SEARCH or CYCLE clause, the right side of the UNION must be a SELECT",
                    );
                }
            }
        }
        if let Some(sc) = search {
            let cols = super::clause::string_list(&sc.search_col_list);
            let mut seen: Vec<&String> = Vec::new();
            for c in &cols {
                if !cte.ctecolnames.contains(c) {
                    return pg_error(
                        "42601",
                        format!("search column \"{c}\" not in WITH query column list"),
                    );
                }
                if seen.contains(&c) {
                    return pg_error(
                        "42701",
                        format!("search column \"{c}\" specified more than once"),
                    );
                }
                seen.push(c);
            }
            if cte.ctecolnames.contains(&sc.search_seq_column) {
                return pg_error(
                    "42601",
                    format!(
                        "search sequence column name \"{}\" already used in WITH query column list",
                        sc.search_seq_column
                    ),
                );
            }
            cte.search_clause = Some(CteSearchClause {
                search_col_list: cols,
                search_breadth_first: sc.search_breadth_first,
                search_seq_column: sc.search_seq_column.clone(),
                location: sc.location,
            });
        }
        if let Some(cc) = cycle {
            let cols = super::clause::string_list(&cc.cycle_col_list);
            let mut seen: Vec<&String> = Vec::new();
            for c in &cols {
                if !cte.ctecolnames.contains(c) {
                    return pg_error(
                        "42601",
                        format!("cycle column \"{c}\" not in WITH query column list"),
                    );
                }
                if seen.contains(&c) {
                    return pg_error(
                        "42701",
                        format!("cycle column \"{c}\" specified more than once"),
                    );
                }
                seen.push(c);
            }
            if cte.ctecolnames.contains(&cc.cycle_mark_column) {
                return pg_error(
                    "42601",
                    format!(
                        "cycle mark column name \"{}\" already used in WITH query column list",
                        cc.cycle_mark_column
                    ),
                );
            }
            if cte.ctecolnames.contains(&cc.cycle_path_column) {
                return pg_error(
                    "42601",
                    format!(
                        "cycle path column name \"{}\" already used in WITH query column list",
                        cc.cycle_path_column
                    ),
                );
            }
            if cc.cycle_mark_column == cc.cycle_path_column {
                return pg_error(
                    "42601",
                    "cycle mark column name and cycle path column name are the same",
                );
            }
        }
        if let (Some(sc), Some(cc)) = (search, cycle) {
            if sc.search_seq_column == cc.cycle_mark_column {
                return pg_error(
                    "42601",
                    "search sequence column name and cycle mark column name are the same",
                );
            }
            if sc.search_seq_column == cc.cycle_path_column {
                return pg_error(
                    "42601",
                    "search sequence column name and cycle path column name are the same",
                );
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
        return Err(AnalysisError::new(format!(
            "WITH query \"{}\" has {varattno} columns available but {numaliases} columns specified",
            cte.ctename
        )));
    }
    Ok(())
}

const LCS_FORUPDATE: i32 = 4;
/// `ACL_SELECT_FOR_UPDATE` (`ACL_UPDATE`).
const ACL_SELECT_FOR_UPDATE: u32 = 1 << 2;

/// `LCS_asString`.
fn lcs_as_string(strength: i32) -> &'static str {
    match strength {
        1 => "FOR KEY SHARE",
        2 => "FOR SHARE",
        3 => "FOR NO KEY UPDATE",
        _ => "FOR UPDATE",
    }
}

/// `CheckSelectLocking`.
fn check_select_locking(q: &Query, strength: i32) -> R<()> {
    let what = lcs_as_string(strength);
    let refuse = |m: &str| pg_error("0A000", format!("{what} is not allowed with {m}"));
    if q.set_operations.is_some() {
        return refuse("UNION/INTERSECT/EXCEPT");
    }
    if !q.distinct_clause.is_empty() {
        return refuse("DISTINCT clause");
    }
    if !q.group_clause.is_empty() || !q.grouping_sets.is_empty() {
        return refuse("GROUP BY clause");
    }
    if q.having_qual.is_some() {
        return refuse("HAVING clause");
    }
    if q.has_aggs {
        return refuse("aggregate functions");
    }
    if q.has_window_funcs {
        return refuse("window functions");
    }
    if q.has_target_srfs {
        return refuse("set-returning functions in the target list");
    }
    Ok(())
}

/// `transformLockingClause`: `rels` empty locks every relation of the
/// query (and of its subqueries, pushed down).
fn transform_locking_clause(
    q: &mut Query,
    rels: &[pb::RangeVar],
    strength: i32,
    wait_policy: i32,
    pushed_down: bool,
) -> R<()> {
    check_select_locking(q, strength)?;
    let what = lcs_as_string(strength);
    if rels.is_empty() {
        for i in 0..q.rtable.len() {
            if !q.rtable[i].in_from_cl {
                continue;
            }
            match &q.rtable[i].kind {
                RteKind::Relation { .. } => {
                    apply_locking_clause(q, i as u32 + 1, strength, wait_policy, pushed_down);
                    q.rtable[i].required_perms |= ACL_SELECT_FOR_UPDATE;
                }
                RteKind::Subquery { .. } => {
                    apply_locking_clause(q, i as u32 + 1, strength, wait_policy, pushed_down);
                    if let RteKind::Subquery { subquery, .. } = &mut q.rtable[i].kind {
                        transform_locking_clause(subquery, &[], strength, wait_policy, true)?;
                    }
                }
                _ => {}
            }
        }
        return Ok(());
    }
    for rv in rels {
        if !rv.catalogname.is_empty() || !rv.schemaname.is_empty() {
            return pg_error(
                "42601",
                format!("{what} must specify unqualified relation names"),
            );
        }
        let mut found = false;
        for i in 0..q.rtable.len() {
            let rte = &q.rtable[i];
            if !rte.in_from_cl {
                continue;
            }
            let name = match &rte.kind {
                RteKind::Join {
                    join_using_alias, ..
                } if rte.alias.is_none() => match join_using_alias {
                    Some(a) => a.aliasname.clone(),
                    None => continue,
                },
                _ => rte.eref.aliasname.clone(),
            };
            if name != rv.relname {
                continue;
            }
            let refuse =
                |kind: &str| pg_error::<()>("0A000", format!("{what} cannot be applied to {kind}"));
            match &rte.kind {
                RteKind::Relation { .. } => {
                    apply_locking_clause(q, i as u32 + 1, strength, wait_policy, pushed_down);
                    q.rtable[i].required_perms |= ACL_SELECT_FOR_UPDATE;
                }
                RteKind::Subquery { .. } => {
                    apply_locking_clause(q, i as u32 + 1, strength, wait_policy, pushed_down);
                    if let RteKind::Subquery { subquery, .. } = &mut q.rtable[i].kind {
                        transform_locking_clause(subquery, &[], strength, wait_policy, true)?;
                    }
                }
                RteKind::Join { .. } => refuse("a join")?,
                RteKind::Function { .. } => refuse("a function")?,
                RteKind::Values { .. } => refuse("VALUES")?,
                RteKind::Cte { .. } => refuse("a WITH query")?,
                RteKind::Result => {}
            }
            found = true;
            break;
        }
        if !found {
            return pg_error(
                "42P01",
                format!(
                    "relation \"{}\" in {what} clause not found in FROM clause",
                    rv.relname
                ),
            );
        }
    }
    Ok(())
}

/// `applyLockingClause`.
fn apply_locking_clause(
    q: &mut Query,
    rti: u32,
    strength: i32,
    wait_policy: i32,
    pushed_down: bool,
) {
    if !pushed_down {
        q.has_for_update = true;
    }
    if let Some(rc) = q.row_marks.iter_mut().find(|r| r.rti == rti) {
        rc.strength = rc.strength.max(strength);
        rc.wait_policy = rc.wait_policy.max(wait_policy);
        rc.pushed_down &= pushed_down;
        return;
    }
    q.row_marks.push(RowMarkClause {
        rti,
        strength,
        wait_policy,
        pushed_down,
    });
}

/// `OffsetVarNodes(node, offset, 0)` on a query: every Var of this level and
/// every rtindex in the jointree moved by `offset`.
fn offset_var_nodes(q: &mut Query, offset: i32) {
    super::nodefuncs::offset_query(q, offset, 0);
}

/// Parse analysis of a plain SELECT (`transformStmt`), for the refusals
/// PostgreSQL makes before it plans anything: a WITH RECURSIVE whose
/// recursive term changes a column's type or holds an aggregate (RP-295).
/// The analysed tree is thrown away.
pub fn analyze_select(ctx: &AnalysisContext<'_>, source: &str, stmt_index: usize) -> R<()> {
    let parsed = parse_source(source)?;
    let raw = parsed
        .stmts
        .get(stmt_index)
        .ok_or_else(|| AnalysisError::new("no such statement".into()))?;
    let Some(query) = raw.stmt.as_deref() else {
        return unsupported("an empty statement");
    };
    if !matches!(query.node.as_ref(), Some(N::SelectStmt(_))) {
        return unsupported("not a SELECT");
    }
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    a.transform_stmt(query)?;
    a.levels.pop();
    Ok(())
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
        .ok_or_else(|| AnalysisError::new("no such statement".into()))?;
    let Some(N::ViewStmt(v)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE VIEW");
    };
    let query = v
        .query
        .as_deref()
        .ok_or_else(|| AnalysisError::new("a view without a query".into()))?;
    // The grammar lets SELECT INTO through; DefineView does not.
    if matches!(query.node.as_ref(), Some(N::SelectStmt(sel)) if sel.into_clause.is_some()) {
        return pg_error("0A000", "views must not contain SELECT INTO");
    }
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    let mut q = a.transform_stmt(query)?;
    a.levels.pop();
    q.stmt_location = raw.stmt_location;
    q.stmt_len = raw.stmt_len;
    if q.command_type != CMD_SELECT {
        return unsupported("a view over a non-SELECT");
    }
    if q.has_modifying_cte {
        return pg_error(
            "0A000",
            "views must not contain data-modifying statements in WITH",
        );
    }
    // WITH CHECK OPTION needs an automatically updatable view.
    let check_option = v.with_check_option
        > pg_query::protobuf::ViewCheckOption::NoCheckOption as i32
        || v.options
            .iter()
            .any(|o| matches!(o.node.as_ref(), Some(N::DefElem(d)) if d.defname == "check_option"));
    if check_option {
        if let Some(why) = view_query_is_auto_updatable(ctx.catalog, &q, true) {
            return Err(AnalysisError::pg(
                "0A000",
                "WITH CHECK OPTION is supported only on automatically updatable views",
            )
            .with_hint(why));
        }
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
    if aliases.next().is_some() {
        return pg_error(
            "42601",
            "CREATE VIEW specifies more column names than columns",
        );
    }
    // DefineVirtualRelation: a collatable column needs its collation.
    for t in q.target_list.iter().filter(|t| !t.resjunk) {
        let ty = expr_type(ctx.catalog, &t.expr);
        if ctx.catalog.type_is_collatable(ty) && expr_collation(ctx.catalog, &t.expr) == 0 {
            return Err(AnalysisError::pg(
                "42P22",
                format!(
                    "could not determine which collation to use for view column \"{}\"",
                    t.resname.clone().unwrap_or_default()
                ),
            )
            .with_hint("Use the COLLATE clause to set the collation explicitly."));
        }
    }
    let columns: Vec<(String, Oid, Oid)> = q
        .target_list
        .iter()
        .filter(|t| !t.resjunk)
        .map(|t| {
            (
                t.resname.clone().unwrap_or_default(),
                expr_type(ctx.catalog, &t.expr),
                expr_collation(ctx.catalog, &t.expr),
            )
        })
        .collect();
    check_new_relation_columns(ctx.catalog, &columns, ctx.allow_system_table_mods)?;
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
        .ok_or_else(|| AnalysisError::new("no such statement".into()))?;
    let Some(N::CreateTableAsStmt(c)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE MATERIALIZED VIEW");
    };
    if c.objtype != pb::ObjectType::ObjectMatview as i32 {
        return unsupported("not a CREATE MATERIALIZED VIEW");
    }
    let query = c
        .query
        .as_deref()
        .ok_or_else(|| AnalysisError::new("a materialized view without a query".into()))?;
    let mut a = Analyzer::new(ctx);
    a.levels.push(ParseState::new());
    let q = a.transform_stmt(query)?;
    a.levels.pop();
    if q.command_type != CMD_SELECT {
        return unsupported("a materialized view over a non-SELECT");
    }
    if q.has_modifying_cte {
        return pg_error(
            "0A000",
            "materialized views must not use data-modifying statements in WITH",
        );
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
    let columns: Vec<(String, Oid, Oid)> = q
        .target_list
        .iter()
        .filter(|t| !t.resjunk)
        .zip(&colnames)
        .map(|(t, n)| {
            (
                n.clone(),
                expr_type(ctx.catalog, &t.expr),
                expr_collation(ctx.catalog, &t.expr),
            )
        })
        .collect();
    check_new_relation_columns(ctx.catalog, &columns, ctx.allow_system_table_mods)?;
    Ok(store_view_query(q, colnames, matview_relid, 'm'))
}

/// `MergeAttributes`' duplicate check and `CheckAttributeNamesTypes` /
/// `CheckAttributeType` over a new view's or materialized view's columns:
/// `(name, type, collation)`.
fn check_new_relation_columns(
    cat: &Catalog,
    columns: &[(String, Oid, Oid)],
    anyarray_ok: bool,
) -> R<()> {
    for (i, (name, _, _)) in columns.iter().enumerate() {
        if columns[..i].iter().any(|(n, _, _)| n == name) {
            return pg_error(
                "42701",
                format!("column \"{name}\" specified more than once"),
            );
        }
    }
    for (name, ty, coll) in columns {
        check_attribute_type(cat, name, *ty, *coll, &mut Vec::new(), anyarray_ok)?;
    }
    Ok(())
}

/// `CheckAttributeType` (no pseudo-type exceptions: a view's flags are 0).
fn check_attribute_type(
    cat: &Catalog,
    name: &str,
    ty: Oid,
    coll: Oid,
    containing: &mut Vec<Oid>,
    anyarray_ok: bool,
) -> R<()> {
    match cat.typtype(ty) {
        'p' if !(anyarray_ok && ty == ANYARRAYOID) => {
            return pg_error(
                "42P16",
                format!(
                    "column \"{name}\" has pseudo-type {}",
                    cat.format_type_be(ty)
                ),
            );
        }
        'd' => check_attribute_type(cat, name, cat.base_type(ty), coll, containing, anyarray_ok)?,
        'c' => {
            if containing.contains(&ty) {
                return pg_error(
                    "42P16",
                    format!(
                        "composite type {} cannot be made a member of itself",
                        cat.format_type_be(ty)
                    ),
                );
            }
            containing.push(ty);
            let relid = cat.typ_relid(ty);
            let atts: Vec<(String, Oid, Oid)> = cat
                .rel_columns(relid)
                .iter()
                .filter(|a| !a.dropped)
                .map(|a| (a.name.clone(), a.typid, a.collation))
                .collect();
            for (n, t, c) in atts {
                check_attribute_type(cat, &n, t, c, containing, anyarray_ok)?;
            }
            containing.pop();
        }
        'r' => {
            let sub = cat.range_subtype(ty);
            let subcoll = cat
                .data
                .ranges
                .iter()
                .find(|r| r.typid == ty)
                .map_or(0, |r| r.collation);
            check_attribute_type(cat, name, sub, subcoll, containing, anyarray_ok)?;
        }
        'm' => check_attribute_type(
            cat,
            name,
            cat.multirange_range(ty),
            0,
            containing,
            anyarray_ok,
        )?,
        _ => {
            let elem = cat.element_type(ty);
            if elem != 0 {
                check_attribute_type(cat, name, elem, coll, containing, anyarray_ok)?;
            }
        }
    }
    if coll == 0 && cat.type_is_collatable(ty) {
        return Err(AnalysisError::pg(
            "42P16",
            format!(
                "no collation was derived for column \"{name}\" with collatable type {}",
                cat.format_type_be(ty)
            ),
        )
        .with_hint("Use the COLLATE clause to set the collation explicitly."));
    }
    Ok(())
}

/// `view_query_is_auto_updatable`: why the view is not, or `None`.
pub(crate) fn view_query_is_auto_updatable(
    cat: &Catalog,
    q: &Query,
    check_cols: bool,
) -> Option<&'static str> {
    if !q.distinct_clause.is_empty() {
        return Some("Views containing DISTINCT are not automatically updatable.");
    }
    if !q.group_clause.is_empty() || !q.grouping_sets.is_empty() {
        return Some("Views containing GROUP BY are not automatically updatable.");
    }
    if q.having_qual.is_some() {
        return Some("Views containing HAVING are not automatically updatable.");
    }
    if q.set_operations.is_some() {
        return Some(
            "Views containing UNION, INTERSECT, or EXCEPT are not automatically updatable.",
        );
    }
    if !q.cte_list.is_empty() {
        return Some("Views containing WITH are not automatically updatable.");
    }
    if q.limit_offset.is_some() || q.limit_count.is_some() {
        return Some("Views containing LIMIT or OFFSET are not automatically updatable.");
    }
    if q.has_aggs {
        return Some("Views that return aggregate functions are not automatically updatable.");
    }
    if q.has_window_funcs {
        return Some("Views that return window functions are not automatically updatable.");
    }
    if q.has_target_srfs {
        return Some("Views that return set-returning functions are not automatically updatable.");
    }
    let single =
        "Views that do not select from a single table or view are not automatically updatable.";
    let fromlist = q
        .jointree
        .as_ref()
        .map_or(&[][..], |j| j.fromlist.as_slice());
    let [JoinTreeNode::RangeTblRef(rti)] = fromlist else {
        return Some(single);
    };
    let Some(base) = q.rtable.get((*rti - 1) as usize) else {
        return Some(single);
    };
    match &base.kind {
        RteKind::Relation {
            relkind: 'r' | 'f' | 'v' | 'p',
            ..
        } => {}
        _ => return Some(single),
    }
    let _ = cat;
    if check_cols {
        let updatable = q.target_list.iter().any(|t| {
            !t.resjunk
                && matches!(&t.expr, Node::Var(v)
                    if v.varno == *rti && v.varlevelsup == 0 && v.varattno > 0)
        });
        if !updatable {
            return Some("Views that have no updatable columns are not automatically updatable.");
        }
    }
    None
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
        .ok_or_else(|| AnalysisError::new("no such statement".into()))?;
    let Some(N::RuleStmt(r)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return unsupported("not a CREATE RULE");
    };
    let event = r.event - 1;
    let rel = ctx
        .catalog
        .rel(relid)
        .ok_or_else(|| AnalysisError::new(format!("relation {relid} does not exist")))?;
    let relkind = rel.kind;
    let relname = rel.name.clone();
    if relkind == 'm' {
        return pg_error("0A000", "rules on materialized views are not supported");
    }
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
        return pg_error(
            "42P17",
            "rule WHERE condition cannot contain references to other relations",
        );
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
            // No conditional utility actions: there is no way to run them
            // conditionally.
            if top.command_type == CMD_UTILITY && qual.is_some() {
                return pg_error(
                    "42P17",
                    "rules with WHERE conditions can only have SELECT, INSERT, UPDATE, or DELETE actions",
                );
            }
            if get_insert_select(&top).set_operations.is_some() && qual.is_some() {
                return pg_error(
                    "0A000",
                    "conditional UNION/INTERSECT/EXCEPT statements are not implemented",
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
                CMD_SELECT if has_old => return pg_error("42P17", "ON SELECT rule cannot use OLD"),
                CMD_SELECT if has_new => return pg_error("42P17", "ON SELECT rule cannot use NEW"),
                CMD_INSERT if has_old => return pg_error("42P17", "ON INSERT rule cannot use OLD"),
                CMD_DELETE if has_new => return pg_error("42P17", "ON DELETE rule cannot use NEW"),
                _ => {}
            }
            // OLD and NEW would be outer references of a WITH query.
            let cte_uses = |q: &Query, n: i32| {
                q.cte_list
                    .iter()
                    .any(|c| super::nodefuncs::range_table_entry_used_query(&c.ctequery, n, 1))
            };
            if cte_uses(&top, 1) || cte_uses(get_insert_select(&top), 1) {
                return pg_error("0A000", "cannot refer to OLD within WITH query");
            }
            if cte_uses(&top, 2) || cte_uses(get_insert_select(&top), 2) {
                return pg_error("0A000", "cannot refer to NEW within WITH query");
            }
            if has_old || (has_new && event == CMD_UPDATE) {
                let sub_qry = get_insert_select_mut(&mut top);
                if sub_qry.set_operations.is_some() {
                    return pg_error(
                        "0A000",
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
    define_query_rewrite_checks(
        ctx,
        &r.rulename,
        &relname,
        relid,
        event,
        r.instead,
        qual.is_some(),
        r.replace,
        &actions,
    )?;
    Ok(RuleTrees {
        ev_qual: super::out::node_to_string(qual.as_ref()),
        ev_action: super::out::queries_to_string(&actions),
    })
}

/// `DefineQueryRewrite`'s checks of a rule before it is stored.
#[allow(clippy::too_many_arguments)]
fn define_query_rewrite_checks(
    ctx: &AnalysisContext<'_>,
    rulename: &str,
    relname: &str,
    relid: Oid,
    event: i32,
    is_instead: bool,
    has_qual: bool,
    replace: bool,
    actions: &[Query],
) -> R<()> {
    let cat = ctx.catalog;
    let relkind = cat.rel(relid).map_or('r', |r| r.kind);
    if !matches!(relkind, 'r' | 'm' | 'v' | 'p') {
        return pg_error("42809", format!("relation \"{relname}\" cannot have rules"))
            .map_err(|e| e.with_detail(relkind_detail(relkind)));
    }
    // No rule actions that modify OLD or NEW.
    for q in actions {
        if q.result_relation == 0 || !std::ptr::eq(q, get_insert_select(q)) {
            continue;
        }
        if q.result_relation == 1 {
            return Err(
                AnalysisError::pg("0A000", "rule actions on OLD are not implemented")
                    .with_hint("Use views or triggers instead."),
            );
        }
        if q.result_relation == 2 {
            return Err(
                AnalysisError::pg("0A000", "rule actions on NEW are not implemented")
                    .with_hint("Use triggers instead."),
            );
        }
    }
    if event == CMD_SELECT {
        // (transformRuleStmt always makes an action, a NOTHING one at least.)
        if actions.is_empty() {
            return Err(AnalysisError::pg(
                "0A000",
                "INSTEAD NOTHING rules on SELECT are not implemented",
            )
            .with_hint("Use views instead."));
        }
        if actions.len() > 1 {
            return pg_error(
                "0A000",
                "multiple actions for rules on SELECT are not implemented",
            );
        }
        let q = &actions[0];
        if !is_instead || q.command_type != CMD_SELECT {
            return pg_error("0A000", "rules on SELECT must have action INSTEAD SELECT");
        }
        if q.has_modifying_cte {
            return pg_error(
                "0A000",
                "rules on SELECT must not contain data-modifying statements in WITH",
            );
        }
        if has_qual {
            return pg_error(
                "0A000",
                "event qualifications are not implemented for rules on SELECT",
            );
        }
        check_rule_result_list(cat, &q.target_list, relid, true, relkind != 'm')?;
        // There must not be another ON SELECT rule already.
        if !replace && relkind == 'v' {
            return pg_error("55000", format!("\"{relname}\" is already a view"));
        }
        // The rule must be named _RETURN (or pre-7.3 pg_dump's
        // `_RET<view name>`, which is renamed).
        if rulename != "_RETURN" {
            let prefix_len = 64 - 4 - 4;
            let after: String = rulename.chars().skip(4).take(prefix_len).collect();
            let rel: String = relname.chars().take(prefix_len).collect();
            if !rulename.starts_with("_RET") || after != rel {
                return pg_error(
                    "42P17",
                    format!("view rule for \"{relname}\" must be named \"_RETURN\""),
                );
            }
        }
    } else {
        let mut have_returning = false;
        for q in actions {
            if q.returning_list.is_empty() {
                continue;
            }
            if have_returning {
                return pg_error("0A000", "cannot have multiple RETURNING lists in a rule");
            }
            have_returning = true;
            if has_qual {
                return pg_error(
                    "0A000",
                    "RETURNING lists are not supported in conditional rules",
                );
            }
            if !is_instead {
                return pg_error(
                    "0A000",
                    "RETURNING lists are not supported in non-INSTEAD rules",
                );
            }
            check_rule_result_list(cat, &q.returning_list, relid, false, false)?;
        }
        if rulename == "_RETURN" {
            return pg_error(
                "42P17",
                format!("non-view rule for \"{relname}\" must not be named \"_RETURN\""),
            );
        }
    }
    Ok(())
}

/// `errdetail_relkind_not_supported`.
fn relkind_detail(relkind: char) -> String {
    let what = match relkind {
        'r' => "tables",
        'i' => "indexes",
        'S' => "sequences",
        't' => "TOAST tables",
        'v' => "views",
        'm' => "materialized views",
        'c' => "composite types",
        'f' => "foreign tables",
        'p' => "partitioned tables",
        _ => "partitioned indexes",
    };
    format!("This operation is not supported for {what}.")
}

/// `checkRuleResultList`: a SELECT rule's target list or a RETURNING list
/// against the relation's columns.
fn check_rule_result_list(
    cat: &Catalog,
    tlist: &[TargetEntry],
    relid: Oid,
    is_select: bool,
    require_name_match: bool,
) -> R<()> {
    let cols: Vec<&AttRow> = cat.rel_columns(relid);
    let mut i = 0usize;
    for t in tlist.iter().filter(|t| !t.resjunk) {
        i += 1;
        let Some(att) = cols.get(i - 1) else {
            return pg_error(
                "42P17",
                if is_select {
                    "SELECT rule's target list has too many entries"
                } else {
                    "RETURNING list has too many entries"
                },
            );
        };
        if att.dropped {
            return pg_error(
                "0A000",
                if is_select {
                    "cannot convert relation containing dropped columns to view"
                } else {
                    "cannot create a RETURNING list for a relation containing dropped columns"
                },
            );
        }
        let resname = t.resname.clone().unwrap_or_default();
        if require_name_match && resname != att.name {
            return Err(AnalysisError::pg(
                "42P17",
                format!(
                    "SELECT rule's target entry {i} has different column name from column \"{}\"",
                    att.name
                ),
            )
            .with_detail(format!("SELECT target entry is named \"{resname}\".")));
        }
        let t_type = expr_type(cat, &t.expr);
        let (what, list) = if is_select {
            ("SELECT rule's target entry", "SELECT target entry")
        } else {
            ("RETURNING list's entry", "RETURNING list entry")
        };
        if att.typid != t_type {
            return Err(AnalysisError::pg(
                "42P17",
                format!("{what} {i} has different type from column \"{}\"", att.name),
            )
            .with_detail(format!(
                "{list} has type {}, but column has type {}.",
                cat.format_type_be(t_type),
                cat.format_type_be(att.typid)
            )));
        }
        let t_typmod = expr_typmod(cat, &t.expr);
        if att.typmod != t_typmod && att.typmod != -1 && t_typmod != -1 {
            return pg_error(
                "42P17",
                format!("{what} {i} has different size from column \"{}\"", att.name),
            );
        }
    }
    if i != cols.len() {
        return pg_error(
            "42P17",
            if is_select {
                "SELECT rule's target list has too few entries"
            } else {
                "RETURNING list has too few entries"
            },
        );
    }
    Ok(())
}

/// `getInsertSelectQuery`: an INSERT ... SELECT's SELECT, where OLD and NEW
/// were pushed down; the query itself when OLD and NEW are its own first
/// two entries.
fn has_rule_placeholders(q: &Query) -> bool {
    q.rtable.len() >= 2
        && q.rtable[0].eref.aliasname == "old"
        && q.rtable[1].eref.aliasname == "new"
}

fn insert_select_index(q: &Query) -> Option<usize> {
    if q.command_type != CMD_INSERT || has_rule_placeholders(q) {
        return None;
    }
    let j = q.jointree.as_ref()?;
    let [JoinTreeNode::RangeTblRef(i)] = j.fromlist.as_slice() else {
        return None;
    };
    match &q.rtable.get((*i - 1) as usize)?.kind {
        RteKind::Subquery { subquery, .. }
            if subquery.command_type == CMD_SELECT && has_rule_placeholders(subquery) =>
        {
            Some((*i - 1) as usize)
        }
        _ => None,
    }
}

fn get_insert_select(q: &Query) -> &Query {
    match insert_select_index(q) {
        Some(i) => match &q.rtable[i].kind {
            RteKind::Subquery { subquery, .. } => subquery,
            _ => q,
        },
        None => q,
    }
}

fn get_insert_select_mut(q: &mut Query) -> &mut Query {
    match insert_select_index(q) {
        Some(i) => match &mut q.rtable[i].kind {
            RteKind::Subquery { subquery, .. } => subquery,
            _ => unreachable!("checked above"),
        },
        None => q,
    }
}

fn parse_source(source: &str) -> R<pb::ParseResult> {
    pg_query::parse(source)
        .map(|r| r.protobuf)
        .map_err(|e| AnalysisError::new(format!("parse: {e}")))
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
