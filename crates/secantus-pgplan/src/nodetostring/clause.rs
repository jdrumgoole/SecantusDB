//! `parse_clause.c`: FROM, WHERE, LIMIT, ORDER BY, GROUP BY, DISTINCT and
//! WINDOW.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_location, expr_type, expr_typmod};
use super::nodes::*;
use super::out::equal;
use super::parse::{Analyzer, ExprKind, NsColumn, NsItem};
use super::relation::convert_alias;
use super::{unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

/// A FROM function's expressions, names and column definition lists.
type RangeFunctionParts = (Vec<Node>, Vec<String>, Vec<Vec<pb::Node>>);

impl<'a> Analyzer<'a> {
    /// `transformFromClause`.
    pub fn transform_from_clause(&mut self, items: &[pb::Node]) -> R<()> {
        for item in items {
            let (n, _top, mut namespace) = self.transform_from_clause_item(item)?;
            self.check_namespace_conflicts(&self.psr().namespace.clone(), &namespace)?;
            for ns in namespace.iter_mut() {
                ns.lateral_only = true;
                ns.lateral_ok = true;
            }
            self.ps().joinlist.push(n);
            self.ps().namespace.extend(namespace);
        }
        for ns in self.ps().namespace.iter_mut() {
            ns.lateral_only = false;
            ns.lateral_ok = true;
        }
        Ok(())
    }

    /// `checkNameSpaceConflicts`.
    fn check_namespace_conflicts(&self, a: &[NsItem], b: &[NsItem]) -> R<()> {
        for n1 in a.iter().filter(|n| n.rel_visible) {
            for n2 in b.iter().filter(|n| n.rel_visible) {
                if n1.names.aliasname != n2.names.aliasname {
                    continue;
                }
                let r1 = &self.psr().rtable[(n1.rtindex - 1) as usize];
                let r2 = &self.psr().rtable[(n2.rtindex - 1) as usize];
                if let (Some(x), Some(y)) = (r1.relid(), r2.relid()) {
                    if r1.alias.is_none() && r2.alias.is_none() && x != y {
                        continue;
                    }
                }
                return unsupported(format!(
                    "table name \"{}\" specified more than once",
                    n1.names.aliasname
                ));
            }
        }
        Ok(())
    }

    /// `transformFromClauseItem`: (jointree node, top item, namespace).
    fn transform_from_clause_item(
        &mut self,
        n: &pb::Node,
    ) -> R<(JoinTreeNode, NsItem, Vec<NsItem>)> {
        match n.node.as_ref() {
            Some(N::RangeVar(rv)) => {
                if rv.schemaname.is_empty() {
                    if let Some((up, idx)) = self.cte_lookup(&rv.relname) {
                        // transformCTEReference
                        let ns = self.add_rte_for_cte(up, idx, rv, true)?;
                        return Ok((JoinTreeNode::RangeTblRef(ns.rtindex), ns.clone(), vec![ns]));
                    }
                }
                let ns = self.add_rte_for_rangevar(rv, true)?;
                Ok((JoinTreeNode::RangeTblRef(ns.rtindex), ns.clone(), vec![ns]))
            }
            Some(N::RangeSubselect(r)) => {
                let ns = self.transform_range_subselect(r)?;
                Ok((JoinTreeNode::RangeTblRef(ns.rtindex), ns.clone(), vec![ns]))
            }
            Some(N::RangeFunction(r)) => {
                let ns = self.transform_range_function(r)?;
                Ok((JoinTreeNode::RangeTblRef(ns.rtindex), ns.clone(), vec![ns]))
            }
            Some(N::JoinExpr(j)) => self.transform_join(j),
            _ => unsupported("this FROM item"),
        }
    }

    /// `scanNameSpaceForCTE`: the level (counted up from this one) and the
    /// position in its `p_ctenamespace` of the CTE named `name`.
    fn cte_lookup(&self, name: &str) -> Option<(usize, usize)> {
        (0..self.depth()).find_map(|up| {
            self.level_ref(up)
                .ctenamespace
                .iter()
                .position(|c| c.ctename == name)
                .map(|i| (up, i))
        })
    }

    /// `transformRangeSubselect`.
    fn transform_range_subselect(&mut self, r: &pb::RangeSubselect) -> R<NsItem> {
        let alias = r
            .alias
            .as_ref()
            .map(convert_alias)
            .ok_or_else(|| AnalysisError::new("subquery in FROM must have an alias".into()))?;
        let sub = r
            .subquery
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a subquery in FROM".into()))?;
        self.ps().expr_kind = ExprKind::FromSubselect;
        self.ps().lateral_active = r.lateral;
        let locked = {
            let name = alias.aliasname.clone();
            let ps = self.psr();
            ps.locked_from_parent
                || ps.locking_clause.iter().any(|lc| {
                    match lc.node.as_ref() {
                Some(N::LockingClause(l)) => l.locked_rels.is_empty()
                    || l.locked_rels.iter().any(
                        |x| matches!(x.node.as_ref(), Some(N::RangeVar(rv)) if rv.relname == name),
                    ),
                _ => false,
            }
                })
        };
        let q = self.sub_analyze(sub, locked, true);
        self.ps().lateral_active = false;
        self.ps().expr_kind = ExprKind::None;
        let q = q?;
        if q.command_type != CMD_SELECT {
            return unsupported("a non-SELECT subquery in FROM");
        }
        self.add_rte_for_subquery(q, alias, r.lateral, true)
    }

    /// `transformRangeFunction`.
    fn transform_range_function(&mut self, r: &pb::RangeFunction) -> R<NsItem> {
        self.ps().lateral_active = true;
        let result = (|| -> R<RangeFunctionParts> {
            let mut funcexprs = Vec::new();
            let mut funcnames = Vec::new();
            let mut coldeflists = Vec::new();
            for pair in &r.functions {
                let Some(N::List(l)) = pair.node.as_ref() else {
                    return unsupported("a function in FROM");
                };
                let fexpr = l
                    .items
                    .first()
                    .ok_or_else(|| AnalysisError::new("a function in FROM".into()))?;
                let coldef: Vec<pb::Node> = match l.items.get(1).and_then(|n| n.node.as_ref()) {
                    Some(N::List(c)) => c.items.clone(),
                    _ => Vec::new(),
                };
                if let Some(N::FuncCall(fc)) = fexpr.node.as_ref() {
                    let is_unnest = fc.funcname.len() == 1
                        && matches!(fc.funcname[0].node.as_ref(), Some(N::String(s)) if s.sval == "unnest");
                    if is_unnest
                        && fc.args.len() > 1
                        && fc.agg_order.is_empty()
                        && fc.agg_filter.is_none()
                        && fc.over.is_none()
                        && !fc.agg_star
                        && !fc.agg_distinct
                        && !fc.func_variadic
                        && coldef.is_empty()
                    {
                        for arg in &fc.args {
                            let newfc = pb::FuncCall {
                                funcname: vec![string_node("pg_catalog"), string_node("unnest")],
                                args: vec![arg.clone()],
                                funcformat: pb::CoercionForm::CoerceExplicitCall as i32,
                                location: fc.location,
                                ..Default::default()
                            };
                            let node = pb::Node {
                                node: Some(N::FuncCall(Box::new(newfc))),
                            };
                            funcexprs.push(self.transform_expr(&node, ExprKind::FromFunction)?);
                            funcnames.push(super::target::figure_colname(&node));
                            coldeflists.push(Vec::new());
                        }
                        continue;
                    }
                }
                funcexprs.push(self.transform_expr(fexpr, ExprKind::FromFunction)?);
                funcnames.push(super::target::figure_colname(fexpr));
                if !coldef.is_empty() && !r.coldeflist.is_empty() {
                    return unsupported("multiple column definition lists");
                }
                coldeflists.push(coldef);
            }
            Ok((funcexprs, funcnames, coldeflists))
        })();
        self.ps().lateral_active = false;
        let (mut funcexprs, funcnames, mut coldeflists) = result?;
        for f in funcexprs.iter_mut() {
            self.assign_expr_collations(f)?;
        }
        if !r.coldeflist.is_empty() {
            if funcexprs.len() != 1 || r.ordinality {
                return unsupported("a column definition list here");
            }
            coldeflists = vec![r.coldeflist.clone()];
        }
        let is_lateral = r.lateral
            || funcexprs
                .iter()
                .any(|f| super::nodefuncs::contain_vars_of_level(f, 0));
        self.add_rte_for_function(funcnames, funcexprs, coldeflists, r, is_lateral, true)
    }

    /// The JOIN branch of `transformFromClauseItem`.
    fn transform_join(&mut self, j: &pb::JoinExpr) -> R<(JoinTreeNode, NsItem, Vec<NsItem>)> {
        let jointype = j.jointype - 1;
        let larg = j
            .larg
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a join without its left side".into()))?;
        let rarg = j
            .rarg
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a join without its right side".into()))?;
        let (lnode, l_ns, mut l_namespace) = self.transform_from_clause_item(larg)?;
        let lateral_ok = jointype == 0 || jointype == 1;
        for n in l_namespace.iter_mut() {
            n.lateral_only = true;
            n.lateral_ok = lateral_ok;
        }
        let sv_len = self.psr().namespace.len();
        self.ps().namespace.extend(l_namespace.iter().cloned());
        let right = self.transform_from_clause_item(rarg);
        self.ps().namespace.truncate(sv_len);
        let (rnode, r_ns, r_namespace) = right?;
        self.check_namespace_conflicts(&l_namespace, &r_namespace)?;
        let mut my_namespace: Vec<NsItem> = l_namespace.into_iter().chain(r_namespace).collect();
        let l_cols = l_ns.columns.clone();
        let l_colnames = l_ns.names.colnames.clone();
        let r_cols = r_ns.columns.clone();
        let r_colnames = r_ns.names.colnames.clone();
        let mut using: Vec<String> = j
            .using_clause
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        if j.is_natural {
            using.clear();
            for l in &l_colnames {
                if l.is_empty() {
                    continue;
                }
                if r_colnames.iter().any(|r| r == l) {
                    using.push(l.clone());
                }
            }
        }
        let join_using_alias = j.join_using_alias.as_ref().map(|a| {
            let mut a = convert_alias(a);
            a.colnames = using.clone();
            a
        });
        let mut l_colnos = Vec::new();
        let mut r_colnos = Vec::new();
        let mut res_colnames: Vec<String> = Vec::new();
        let mut res_colvars: Vec<Node> = Vec::new();
        let mut res_nscolumns: Vec<NsColumn> = Vec::new();
        let mut quals = None;
        if !using.is_empty() {
            let mut l_usingvars = Vec::new();
            let mut r_usingvars = Vec::new();
            for u in &using {
                if res_colnames.contains(u) {
                    return unsupported(format!(
                        "column name \"{u}\" appears more than once in USING clause"
                    ));
                }
                let find = |names: &[String], side: &str| -> R<usize> {
                    let mut idx = None;
                    for (i, n) in names.iter().enumerate() {
                        if n == u {
                            if idx.is_some() {
                                return unsupported(format!(
                                    "common column name \"{u}\" appears more than once in {side} table"
                                ));
                            }
                            idx = Some(i);
                        }
                    }
                    idx.ok_or_else(|| {
                        AnalysisError::new(format!(
                            "column \"{u}\" specified in USING clause does not exist in {side} table"
                        ))
                    })
                };
                let li = find(&l_colnames, "left")?;
                let ri = find(&r_colnames, "right")?;
                l_colnos.push(li as i32 + 1);
                r_colnos.push(ri as i32 + 1);
                let lv = var_from_nscolumn(&l_cols[li]);
                let rv = var_from_nscolumn(&r_cols[ri]);
                l_usingvars.push(lv.clone());
                r_usingvars.push(rv.clone());
                res_colnames.push(u.clone());
                let (merged, which) = self.build_merged_join_var(jointype, &lv, &rv)?;
                let nscol = match which {
                    1 => l_cols[li].clone(),
                    2 => r_cols[ri].clone(),
                    _ => NsColumn {
                        varno: 0,
                        varattno: res_nscolumns.len() as i32 + 1,
                        vartype: expr_type(self.cat, &merged),
                        vartypmod: expr_typmod(self.cat, &merged),
                        varcollid: expr_collation(self.cat, &merged),
                        varnosyn: 0,
                        varattnosyn: res_nscolumns.len() as i32 + 1,
                        dontexpand: false,
                    },
                };
                res_colvars.push(merged);
                res_nscolumns.push(nscol);
            }
            quals = Some(self.transform_join_using_clause(l_usingvars, r_usingvars)?);
        } else if let Some(q) = j.quals.as_deref() {
            // `transformJoinOnClause`.
            let mut visible = my_namespace.clone();
            for n in visible.iter_mut() {
                n.lateral_only = false;
                n.lateral_ok = true;
            }
            let sv = std::mem::replace(&mut self.ps().namespace, visible);
            let r = self.transform_where_clause(Some(q), ExprKind::JoinOn);
            self.ps().namespace = sv;
            quals = r?;
        }
        // extractRemainingColumns, left then right.
        for (cols, names, colnos) in [
            (&l_cols, &l_colnames, &mut l_colnos),
            (&r_cols, &r_colnames, &mut r_colnos),
        ] {
            let prev: Vec<i32> = colnos.clone();
            for (i, name) in names.iter().enumerate() {
                let attnum = i as i32 + 1;
                if !name.is_empty() && !prev.contains(&attnum) {
                    colnos.push(attnum);
                    res_colnames.push(name.clone());
                    res_colvars.push(Node::Var(var_from_nscolumn(&cols[i])));
                    res_nscolumns.push(cols[i].clone());
                }
            }
        }
        let alias = j.alias.as_ref().map(convert_alias);
        let nmerged = using.len() as i32;
        let ns = self.add_rte_for_join(
            res_colnames,
            res_nscolumns,
            jointype,
            nmerged,
            res_colvars,
            l_colnos,
            r_colnos,
            join_using_alias.clone(),
            alias.clone(),
            true,
        );
        let rtindex = ns.rtindex;
        let mut ns = ns;
        for (k, c) in ns.columns.iter_mut().enumerate() {
            if c.varno == 0 {
                c.varno = rtindex;
            }
            if c.varnosyn == 0 {
                c.varnosyn = rtindex;
            }
            if alias.is_some() {
                c.varnosyn = rtindex;
                c.varattnosyn = k as i32 + 1;
            }
        }
        let jexpr = JoinExpr {
            jointype,
            is_natural: j.is_natural,
            larg: lnode.clone(),
            rarg: rnode.clone(),
            using_clause: using.clone(),
            join_using_alias: join_using_alias.clone(),
            quals,
            alias: alias.clone(),
            rtindex,
        };
        {
            let ps = self.ps();
            while ps.joinexprs.len() + 1 < rtindex as usize {
                ps.joinexprs.push(None);
            }
            ps.joinexprs.push(Some((lnode, rnode)));
        }
        if let Some(ua) = &join_using_alias {
            let jns = NsItem {
                names: ua.clone(),
                is_eref: false,
                rtindex,
                columns: ns.columns.clone(),
                rel_visible: true,
                cols_visible: true,
                lateral_only: false,
                lateral_ok: true,
            };
            self.check_namespace_conflicts(std::slice::from_ref(&jns), &my_namespace)?;
            my_namespace.push(jns);
        }
        if alias.is_some() {
            my_namespace.clear();
        } else {
            for n in my_namespace.iter_mut() {
                n.cols_visible = false;
            }
        }
        ns.rel_visible = alias.is_some();
        ns.cols_visible = true;
        ns.lateral_only = false;
        ns.lateral_ok = true;
        my_namespace.push(ns.clone());
        Ok((JoinTreeNode::JoinExpr(Box::new(jexpr)), ns, my_namespace))
    }

    /// `buildMergedJoinVar`: the merged column, and which input it is (1
    /// left, 2 right, 0 neither).
    fn build_merged_join_var(&mut self, jointype: i32, l: &Var, r: &Var) -> R<(Node, u8)> {
        let pair = vec![Node::Var(l.clone()), Node::Var(r.clone())];
        let outtype = self.select_common_type(&pair, Some("JOIN/USING"))?;
        let outtypmod = self.select_common_typmod(&pair, outtype);
        let coerce_side = |me: &mut Self, v: &Var| -> R<(Node, bool)> {
            if v.vartype != outtype {
                Ok((
                    me.coerce_type(
                        Node::Var(v.clone()),
                        v.vartype,
                        outtype,
                        outtypmod,
                        CoercionContext::Implicit,
                        CoercionForm::ImplicitCast,
                        -1,
                    )?,
                    false,
                ))
            } else if v.vartypmod != outtypmod {
                Ok((
                    Node::RelabelType(Box::new(RelabelType {
                        arg: Node::Var(v.clone()),
                        resulttype: outtype,
                        resulttypmod: outtypmod,
                        resultcollid: 0,
                        relabelformat: CoercionForm::ImplicitCast,
                        location: -1,
                    })),
                    false,
                ))
            } else {
                Ok((Node::Var(v.clone()), true))
            }
        };
        let (lnode, l_same) = coerce_side(self, l)?;
        let (rnode, r_same) = coerce_side(self, r)?;
        let (mut res, which) = match jointype {
            0 => {
                if matches!(lnode, Node::Var(_)) {
                    (lnode, if l_same { 1 } else { 0 })
                } else if matches!(rnode, Node::Var(_)) {
                    (rnode, if r_same { 2 } else { 0 })
                } else {
                    (lnode, 0)
                }
            }
            1 => (lnode, if l_same { 1 } else { 0 }),
            3 => (rnode, if r_same { 2 } else { 0 }),
            2 => (
                Node::CoalesceExpr(Box::new(CoalesceExpr {
                    coalescetype: outtype,
                    coalescecollid: 0,
                    args: vec![lnode, rnode],
                    location: -1,
                })),
                0,
            ),
            _ => return unsupported("this join type"),
        };
        self.assign_expr_collations(&mut res)?;
        Ok((res, which))
    }

    /// `transformJoinUsingClause`.
    fn transform_join_using_clause(&mut self, lvars: Vec<Var>, rvars: Vec<Var>) -> R<Node> {
        let mut andargs = Vec::new();
        self.ps().expr_kind = ExprKind::JoinUsing;
        for (l, r) in lvars.iter().zip(rvars.iter()) {
            self.mark_var_for_select_priv(l);
            self.mark_var_for_select_priv(r);
            let op = self.make_op(
                &["=".to_string()],
                Some(Node::Var(l.clone())),
                Node::Var(r.clone()),
                -1,
            )?;
            andargs.push(op);
        }
        let result = if andargs.len() == 1 {
            andargs.pop().expect("one")
        } else {
            let mut args = Vec::new();
            for a in andargs {
                args.push(self.coerce_to_boolean(a, "AND")?);
            }
            Node::BoolExpr(Box::new(BoolExpr {
                boolop: BoolOp::And,
                args,
                location: -1,
            }))
        };
        self.ps().expr_kind = ExprKind::None;
        self.coerce_to_boolean(result, "JOIN/USING")
    }

    /// `transformWhereClause`.
    pub fn transform_where_clause(
        &mut self,
        clause: Option<&pb::Node>,
        kind: ExprKind,
    ) -> R<Option<Node>> {
        let Some(c) = clause else {
            return Ok(None);
        };
        let q = self.transform_expr(c, kind)?;
        Ok(Some(self.coerce_to_boolean(q, "WHERE")?))
    }

    /// `transformLimitClause`.
    pub fn transform_limit_clause(
        &mut self,
        clause: Option<&pb::Node>,
        kind: ExprKind,
    ) -> R<Option<Node>> {
        let Some(c) = clause else {
            return Ok(None);
        };
        let q = self.transform_expr(c, kind)?;
        let q = self.coerce_to_specific_type(q, INT8OID, "LIMIT")?;
        if super::nodefuncs::contain_vars_of_level(&q, 0) {
            return unsupported("argument of LIMIT must not contain variables");
        }
        Ok(Some(q))
    }

    /// `findTargetlistEntrySQL92`: the index of the matching entry.
    pub(crate) fn find_targetlist_entry_sql92(
        &mut self,
        node: &pb::Node,
        tlist: &mut Vec<TargetEntry>,
        kind: ExprKind,
    ) -> R<usize> {
        if let Some(N::ColumnRef(c)) = node.node.as_ref() {
            if c.fields.len() == 1 {
                if let Some(N::String(s)) = c.fields[0].node.as_ref() {
                    let mut name = Some(s.sval.clone());
                    if kind == ExprKind::GroupBy {
                        // A FROM column wins in GROUP BY. The lookup marks
                        // nothing for privileges here: `colNameToVar` does,
                        // but the column is marked again when the item is
                        // transformed below.
                        if self.col_name_to_var(&s.sval, true, c.location)?.is_some() {
                            name = None;
                        }
                    }
                    if let Some(name) = name {
                        let mut found: Option<usize> = None;
                        for (i, t) in tlist.iter().enumerate() {
                            if !t.resjunk && t.resname.as_deref() == Some(name.as_str()) {
                                match found {
                                    Some(f) => {
                                        if !equal(&tlist[f].expr, &t.expr) {
                                            return unsupported(format!("\"{name}\" is ambiguous"));
                                        }
                                    }
                                    None => found = Some(i),
                                }
                            }
                        }
                        if let Some(f) = found {
                            return Ok(f);
                        }
                    }
                }
            }
        }
        if let Some(N::AConst(ac)) = node.node.as_ref() {
            let Some(pb::a_const::Val::Ival(iv)) = ac.val.as_ref() else {
                return unsupported("non-integer constant in ORDER BY");
            };
            let target = iv.ival;
            let mut pos = 0;
            for (i, t) in tlist.iter().enumerate() {
                if !t.resjunk {
                    pos += 1;
                    if pos == target {
                        return Ok(i);
                    }
                }
            }
            return unsupported(format!("position {target} is not in select list"));
        }
        self.find_targetlist_entry_sql99(node, tlist, kind)
    }

    /// `findTargetlistEntrySQL99`.
    pub(crate) fn find_targetlist_entry_sql99(
        &mut self,
        node: &pb::Node,
        tlist: &mut Vec<TargetEntry>,
        kind: ExprKind,
    ) -> R<usize> {
        let expr = self.transform_expr(node, kind)?;
        for (i, t) in tlist.iter().enumerate() {
            let texpr = super::nodefuncs::strip_implicit_coercions(&t.expr);
            if equal(&expr, texpr) {
                return Ok(i);
            }
        }
        let resno = self.ps().next_resno;
        self.ps().next_resno += 1;
        tlist.push(TargetEntry {
            expr,
            resno,
            resname: None,
            ressortgroupref: 0,
            resorigtbl: 0,
            resorigcol: 0,
            resjunk: true,
        });
        Ok(tlist.len() - 1)
    }

    /// `transformSortClause`.
    pub fn transform_sort_clause(
        &mut self,
        orderlist: &[pb::Node],
        tlist: &mut Vec<TargetEntry>,
        kind: ExprKind,
        sql99: bool,
    ) -> R<Vec<SortGroupClause>> {
        let mut sortlist = Vec::new();
        for item in orderlist {
            let Some(N::SortBy(sb)) = item.node.as_ref() else {
                return unsupported("an ORDER BY item");
            };
            let node = sb
                .node
                .as_deref()
                .ok_or_else(|| AnalysisError::new("an empty ORDER BY item".into()))?;
            let idx = if sql99 {
                self.find_targetlist_entry_sql99(node, tlist, kind)?
            } else {
                self.find_targetlist_entry_sql92(node, tlist, kind)?
            };
            self.add_target_to_sort_list(idx, &mut sortlist, tlist, sb)?;
        }
        Ok(sortlist)
    }

    /// `addTargetToSortList`.
    pub fn add_target_to_sort_list(
        &mut self,
        idx: usize,
        sortlist: &mut Vec<SortGroupClause>,
        tlist: &mut [TargetEntry],
        sb: &pb::SortBy,
    ) -> R<()> {
        let mut restype = expr_type(self.cat, &tlist[idx].expr);
        if restype == UNKNOWNOID {
            let e = tlist[idx].expr.clone();
            tlist[idx].expr = self.coerce_type(
                e,
                restype,
                TEXTOID,
                -1,
                CoercionContext::Implicit,
                CoercionForm::ImplicitCast,
                -1,
            )?;
            restype = TEXTOID;
        }
        let dir = sb.sortby_dir - 1;
        let (sortop, eqop, hashable, reverse) = match dir {
            0 | 1 => {
                let (lt, eq, _, h) = self.cat.sort_group_operators(restype);
                if lt == 0 || eq == 0 {
                    return unsupported("could not identify an ordering operator");
                }
                (lt, eq, h, false)
            }
            2 => {
                let (_, eq, gt, h) = self.cat.sort_group_operators(restype);
                if gt == 0 || eq == 0 {
                    return unsupported("could not identify an ordering operator");
                }
                (gt, eq, h, true)
            }
            3 => {
                let names = string_list(&sb.use_op);
                let op = self.compatible_oper_opid(&names, restype, restype)?;
                let (eq, reverse) = self
                    .cat
                    .equality_op_for_ordering_op(op)
                    .ok_or_else(|| AnalysisError::new("not a valid ordering operator".into()))?;
                let h = self.cat.op_hashjoinable(eq, restype);
                (op, eq, h, reverse)
            }
            _ => return unsupported("this sort direction"),
        };
        if !target_is_in_sort_list(self.cat, &tlist[idx], sortop, sortlist) {
            let r = assign_sort_group_ref(idx, tlist);
            let nulls_first = match sb.sortby_nulls - 1 {
                1 => true,
                2 => false,
                _ => reverse,
            };
            sortlist.push(SortGroupClause {
                tle_sort_group_ref: r,
                eqop,
                sortop,
                nulls_first,
                hashable,
            });
        }
        Ok(())
    }

    /// `addTargetToGroupList`.
    pub(crate) fn add_target_to_group_list(
        &mut self,
        idx: usize,
        grouplist: &mut Vec<SortGroupClause>,
        tlist: &mut [TargetEntry],
    ) -> R<()> {
        let mut restype = expr_type(self.cat, &tlist[idx].expr);
        if restype == UNKNOWNOID {
            let e = tlist[idx].expr.clone();
            tlist[idx].expr = self.coerce_type(
                e,
                restype,
                TEXTOID,
                -1,
                CoercionContext::Implicit,
                CoercionForm::ImplicitCast,
                -1,
            )?;
            restype = TEXTOID;
        }
        if !target_is_in_sort_list(self.cat, &tlist[idx], 0, grouplist) {
            let (lt, eq, _, hashable) = self.cat.sort_group_operators(restype);
            if eq == 0 {
                return unsupported("could not identify an equality operator");
            }
            let r = assign_sort_group_ref(idx, tlist);
            grouplist.push(SortGroupClause {
                tle_sort_group_ref: r,
                eqop: eq,
                sortop: lt,
                nulls_first: false,
                hashable,
            });
        }
        Ok(())
    }

    /// `transformDistinctClause`.
    pub fn transform_distinct_clause(
        &mut self,
        tlist: &mut [TargetEntry],
        sort_clause: &[SortGroupClause],
        is_agg: bool,
    ) -> R<Vec<SortGroupClause>> {
        let mut result = Vec::new();
        for scl in sort_clause {
            let Some(t) = tlist
                .iter()
                .find(|t| t.ressortgroupref == scl.tle_sort_group_ref)
            else {
                return unsupported("a sort clause without its target");
            };
            if t.resjunk {
                return unsupported(if is_agg {
                    "in an aggregate with DISTINCT, ORDER BY expressions must appear in argument list"
                } else {
                    "for SELECT DISTINCT, ORDER BY expressions must appear in select list"
                });
            }
            result.push(scl.clone());
        }
        for i in 0..tlist.len() {
            if tlist[i].resjunk {
                continue;
            }
            self.add_target_to_group_list(i, &mut result, tlist)?;
        }
        if result.is_empty() {
            return unsupported("SELECT DISTINCT must have at least one column");
        }
        Ok(result)
    }

    /// `transformDistinctOnClause`.
    pub fn transform_distinct_on_clause(
        &mut self,
        distinctlist: &[pb::Node],
        tlist: &mut Vec<TargetEntry>,
        sort_clause: &[SortGroupClause],
    ) -> R<Vec<SortGroupClause>> {
        let mut refs = Vec::new();
        for d in distinctlist {
            let idx = self.find_targetlist_entry_sql92(d, tlist, ExprKind::DistinctOn)?;
            refs.push(assign_sort_group_ref(idx, tlist));
        }
        let mut result = Vec::new();
        let mut skipped = false;
        for scl in sort_clause {
            if refs.contains(&scl.tle_sort_group_ref) {
                if skipped {
                    return unsupported(
                        "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
                    );
                }
                result.push(scl.clone());
            } else {
                skipped = true;
            }
        }
        for r in refs {
            let idx = tlist
                .iter()
                .position(|t| t.ressortgroupref == r)
                .ok_or_else(|| AnalysisError::new("a DISTINCT ON target".into()))?;
            if target_is_in_sort_list(self.cat, &tlist[idx], 0, &result) {
                continue;
            }
            if skipped {
                return unsupported(
                    "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
                );
            }
            self.add_target_to_group_list(idx, &mut result, tlist)?;
        }
        Ok(result)
    }

    /// `transformFrameOffset`: the offset expression and, for RANGE, the
    /// `in_range` support function chosen for it.
    fn transform_frame_offset(
        &mut self,
        frame_options: i32,
        rangeopfamily: Oid,
        rangeopcintype: Oid,
        clause: Option<&pb::Node>,
    ) -> R<(Option<Node>, Oid)> {
        let Some(clause) = clause else {
            return Ok((None, 0));
        };
        let (node, construct, func) = if frame_options & FRAMEOPTION_ROWS != 0 {
            let n = self.transform_expr(clause, ExprKind::WindowFrameRows)?;
            (self.coerce_to_specific_type(n, INT8OID, "ROWS")?, "ROWS", 0)
        } else if frame_options & FRAMEOPTION_RANGE != 0 {
            let n = self.transform_expr(clause, ExprKind::WindowFrameRange)?;
            let node_type = expr_type(self.cat, &n);
            let preferred = if node_type != UNKNOWNOID {
                node_type
            } else {
                rangeopcintype
            };
            // The AMPROCNUM cache list: the family's procs for this left
            // type, in index order (right type, then number).
            let mut procs: Vec<&AmprocRow> = self
                .cat
                .data
                .amprocs
                .iter()
                .filter(|p| p.family == rangeopfamily && p.lefttype == rangeopcintype)
                .collect();
            procs.sort_by_key(|p| (p.righttype, p.num));
            let (mut nfuncs, mut nmatches) = (0, 0);
            let (mut selected_type, mut selected_func) = (0, 0);
            for p in procs {
                if p.num != BTINRANGE_PROC {
                    continue;
                }
                nfuncs += 1;
                if !self.can_coerce_type(&[node_type], &[p.righttype], CoercionContext::Implicit) {
                    continue;
                }
                nmatches += 1;
                if selected_type != preferred {
                    selected_type = p.righttype;
                    selected_func = p.proc_;
                }
            }
            if nfuncs == 0 || nmatches == 0 || (nmatches != 1 && selected_type != preferred) {
                return Err(AnalysisError::new(
                    "RANGE with offset PRECEDING/FOLLOWING is not supported for this type".into(),
                ));
            }
            (
                self.coerce_to_specific_type(n, selected_type, "RANGE")?,
                "RANGE",
                selected_func,
            )
        } else if frame_options & FRAMEOPTION_GROUPS != 0 {
            let n = self.transform_expr(clause, ExprKind::WindowFrameGroups)?;
            (
                self.coerce_to_specific_type(n, INT8OID, "GROUPS")?,
                "GROUPS",
                0,
            )
        } else {
            return unsupported("a frame offset without a frame mode");
        };
        // checkExprIsVarFree
        if super::nodefuncs::contain_vars_of_level(&node, 0) {
            return Err(AnalysisError::new(format!(
                "argument of {construct} must not contain variables"
            )));
        }
        Ok((Some(node), func))
    }

    /// `transformWindowDefinitions`.
    pub fn transform_window_definitions(
        &mut self,
        windowdefs: &[pb::WindowDef],
        tlist: &mut Vec<TargetEntry>,
    ) -> R<Vec<WindowClause>> {
        let mut result: Vec<WindowClause> = Vec::new();
        for (i, w) in windowdefs.iter().enumerate() {
            let winref = i as u32 + 1;
            let refwc = if !w.refname.is_empty() {
                Some(
                    result
                        .iter()
                        .find(|c| c.name.as_deref() == Some(w.refname.as_str()))
                        .cloned()
                        .ok_or_else(|| {
                            AnalysisError::new(format!("window \"{}\" does not exist", w.refname))
                        })?,
                )
            } else {
                None
            };
            let order =
                self.transform_sort_clause(&w.order_clause, tlist, ExprKind::WindowOrder, true)?;
            let (partition, _) = self.transform_group_clause(
                &w.partition_clause,
                false,
                tlist,
                &order,
                ExprKind::WindowPartition,
                true,
            )?;
            let (partition_clause, order_clause, copied) = match &refwc {
                Some(r) => {
                    if !partition.is_empty() {
                        return unsupported("cannot override PARTITION BY clause");
                    }
                    if !order.is_empty() && !r.order_clause.is_empty() {
                        return unsupported("cannot override ORDER BY clause");
                    }
                    if order.is_empty() {
                        (r.partition_clause.clone(), r.order_clause.clone(), true)
                    } else {
                        (r.partition_clause.clone(), order, false)
                    }
                }
                None => (partition, order, false),
            };
            let frame_options = w.frame_options;
            // RANGE offset PRECEDING/FOLLOWING requires exactly one ORDER BY
            // column; get its sort opfamily.
            let (mut rangeopfamily, mut rangeopcintype) = (0, 0);
            let (mut in_range_coll, mut in_range_asc, mut in_range_nulls_first) = (0, false, false);
            if frame_options & FRAMEOPTION_RANGE != 0
                && frame_options & (FRAMEOPTION_START_OFFSET | FRAMEOPTION_END_OFFSET) != 0
            {
                if order_clause.len() != 1 {
                    return Err(AnalysisError::new(
                        "RANGE with offset PRECEDING/FOLLOWING requires exactly one ORDER BY column"
                            .into(),
                    ));
                }
                let sortcl = &order_clause[0];
                let sortkey = tlist
                    .iter()
                    .find(|t| t.ressortgroupref == sortcl.tle_sort_group_ref)
                    .map(|t| t.expr.clone())
                    .ok_or_else(|| AnalysisError::new("a window ORDER BY target".into()))?;
                let (fam, intype, strategy) = self
                    .cat
                    .ordering_op_properties(sortcl.sortop)
                    .ok_or_else(|| {
                        AnalysisError::new(format!(
                            "operator {} is not a valid ordering operator",
                            sortcl.sortop
                        ))
                    })?;
                rangeopfamily = fam;
                rangeopcintype = intype;
                in_range_coll = expr_collation(self.cat, &sortkey);
                in_range_asc = strategy == 1;
                in_range_nulls_first = sortcl.nulls_first;
            }
            if frame_options & FRAMEOPTION_GROUPS != 0 && order_clause.is_empty() {
                return Err(AnalysisError::new(
                    "GROUPS mode requires an ORDER BY clause".into(),
                ));
            }
            let (start_offset, start_in_range_func) = self.transform_frame_offset(
                frame_options,
                rangeopfamily,
                rangeopcintype,
                w.start_offset.as_deref(),
            )?;
            let (end_offset, end_in_range_func) = self.transform_frame_offset(
                frame_options,
                rangeopfamily,
                rangeopcintype,
                w.end_offset.as_deref(),
            )?;
            result.push(WindowClause {
                name: (!w.name.is_empty()).then(|| w.name.clone()),
                refname: (!w.refname.is_empty()).then(|| w.refname.clone()),
                partition_clause,
                order_clause,
                frame_options,
                start_offset,
                end_offset,
                start_in_range_func,
                end_in_range_func,
                in_range_coll,
                in_range_asc,
                in_range_nulls_first,
                winref,
                copied_order: copied,
            });
        }
        Ok(result)
    }
}

/// `assignSortGroupRef`.
pub(crate) fn assign_sort_group_ref(idx: usize, tlist: &mut [TargetEntry]) -> u32 {
    if tlist[idx].ressortgroupref != 0 {
        return tlist[idx].ressortgroupref;
    }
    let max = tlist.iter().map(|t| t.ressortgroupref).max().unwrap_or(0);
    tlist[idx].ressortgroupref = max + 1;
    max + 1
}

/// `targetIsInSortList`.
pub(crate) fn target_is_in_sort_list(
    cat: &Catalog,
    tle: &TargetEntry,
    sortop: Oid,
    list: &[SortGroupClause],
) -> bool {
    let r = tle.ressortgroupref;
    if r == 0 {
        return false;
    }
    list.iter().any(|s| {
        s.tle_sort_group_ref == r
            && (sortop == 0 || sortop == s.sortop || sortop == cat.commutator(s.sortop))
    })
}

/// `buildVarFromNSColumn`.
pub(crate) fn var_from_nscolumn(c: &NsColumn) -> Var {
    Var {
        varno: c.varno,
        varattno: c.varattno,
        vartype: c.vartype,
        vartypmod: c.vartypmod,
        varcollid: c.varcollid,
        varlevelsup: 0,
        varnosyn: c.varnosyn as u32,
        varattnosyn: c.varattnosyn,
        location: -1,
    }
}

pub(crate) fn string_node(s: &str) -> pb::Node {
    pb::Node {
        node: Some(N::String(pb::String {
            sval: s.to_string(),
        })),
    }
}

pub(crate) fn string_list(nodes: &[pb::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

#[allow(dead_code)]
fn location_of(n: &Node) -> i32 {
    expr_location(n)
}

const FRAMEOPTION_RANGE: i32 = 0x2;
const FRAMEOPTION_ROWS: i32 = 0x4;
const FRAMEOPTION_GROUPS: i32 = 0x8;
const FRAMEOPTION_START_OFFSET: i32 = 0x800 | 0x2000;
const FRAMEOPTION_END_OFFSET: i32 = 0x1000 | 0x4000;
/// `BTINRANGE_PROC`.
const BTINRANGE_PROC: i32 = 3;
