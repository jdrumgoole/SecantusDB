//! `parse_target.c`, and the INSERT / UPDATE / DELETE transforms of
//! `analyze.c` a rule action needs.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_location, expr_type, expr_typmod};
use super::nodes::*;
use super::parse::{Analyzer, ExprKind};
use super::relation::{TupDescAttr, FIRST_LOW_INVALID};
use super::{unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

impl<'a> Analyzer<'a> {
    /// `transformTargetList`.
    pub fn transform_target_list(
        &mut self,
        targetlist: &[pb::Node],
        kind: ExprKind,
    ) -> R<Vec<TargetEntry>> {
        let mut out = Vec::new();
        let expand_star = kind != ExprKind::UpdateSource;
        for item in targetlist {
            let Some(N::ResTarget(res)) = item.node.as_ref() else {
                return unsupported("a target list item");
            };
            let val = res
                .val
                .as_deref()
                .ok_or_else(|| AnalysisError("a target without a value".into()))?;
            if expand_star {
                if let Some(N::ColumnRef(c)) = val.node.as_ref() {
                    if matches!(
                        c.fields.last().and_then(|f| f.node.as_ref()),
                        Some(N::AStar(_))
                    ) {
                        out.extend(self.expand_column_ref_star(c, true)?.into_iter().map(
                            |e| match e {
                                Starred::Tle(t) => t,
                                Starred::Expr(_) => unreachable!("target entries asked for"),
                            },
                        ));
                        continue;
                    }
                } else if let Some(N::AIndirection(ind)) = val.node.as_ref() {
                    if matches!(
                        ind.indirection.last().and_then(|f| f.node.as_ref()),
                        Some(N::AStar(_))
                    ) {
                        for e in self.expand_indirection_star(ind, true, kind)? {
                            if let Starred::Tle(t) = e {
                                out.push(t);
                            }
                        }
                        continue;
                    }
                }
            }
            let name = (!res.name.is_empty()).then(|| res.name.clone());
            out.push(self.transform_target_entry(val, None, kind, name, false)?);
        }
        Ok(out)
    }

    /// `transformTargetEntry`.
    pub fn transform_target_entry(
        &mut self,
        node: &pb::Node,
        expr: Option<Node>,
        kind: ExprKind,
        colname: Option<String>,
        resjunk: bool,
    ) -> R<TargetEntry> {
        let expr = match expr {
            Some(e) => e,
            None => self.transform_expr(node, kind)?,
        };
        let colname = if colname.is_none() && !resjunk {
            Some(figure_colname(node))
        } else {
            colname
        };
        let resno = self.ps().next_resno;
        self.ps().next_resno += 1;
        Ok(TargetEntry {
            expr,
            resno,
            resname: colname,
            ressortgroupref: 0,
            resorigtbl: 0,
            resorigcol: 0,
            resjunk,
        })
    }

    /// `transformExpressionList`.
    pub fn transform_expression_list(
        &mut self,
        exprs: &[pb::Node],
        kind: ExprKind,
        allow_default: bool,
    ) -> R<Vec<Node>> {
        let mut out = Vec::new();
        for e in exprs {
            if let Some(N::ColumnRef(c)) = e.node.as_ref() {
                if matches!(
                    c.fields.last().and_then(|f| f.node.as_ref()),
                    Some(N::AStar(_))
                ) {
                    for s in self.expand_column_ref_star(c, false)? {
                        if let Starred::Expr(x) = s {
                            out.push(x);
                        }
                    }
                    continue;
                }
            } else if let Some(N::AIndirection(ind)) = e.node.as_ref() {
                if matches!(
                    ind.indirection.last().and_then(|f| f.node.as_ref()),
                    Some(N::AStar(_))
                ) {
                    for s in self.expand_indirection_star(ind, false, kind)? {
                        if let Starred::Expr(x) = s {
                            out.push(x);
                        }
                    }
                    continue;
                }
            }
            if allow_default {
                if let Some(N::SetToDefault(d)) = e.node.as_ref() {
                    out.push(Node::SetToDefault(SetToDefault {
                        type_id: 0,
                        type_mod: -1,
                        collation: 0,
                        location: d.location,
                    }));
                    continue;
                }
            }
            out.push(self.transform_expr(e, kind)?);
        }
        Ok(out)
    }

    /// `resolveTargetListUnknowns`.
    pub fn resolve_target_list_unknowns(&mut self, tlist: &mut [TargetEntry]) -> R<()> {
        for t in tlist.iter_mut() {
            let restype = expr_type(self.cat, &t.expr);
            if restype == UNKNOWNOID {
                let e = t.expr.clone();
                t.expr = self.coerce_type(
                    e,
                    restype,
                    TEXTOID,
                    -1,
                    CoercionContext::Implicit,
                    CoercionForm::ImplicitCast,
                    -1,
                )?;
            }
        }
        Ok(())
    }

    /// `markTargetListOrigins`.
    pub fn mark_target_list_origins(&self, tlist: &mut [TargetEntry]) {
        for t in tlist.iter_mut() {
            if let Node::Var(v) = &t.expr {
                let v = v.clone();
                self.mark_target_list_origin(t, &v, 0);
            }
        }
    }

    fn mark_target_list_origin(&self, t: &mut TargetEntry, v: &Var, levelsup: usize) {
        let net = v.varlevelsup as usize + levelsup;
        if net >= self.depth() {
            return;
        }
        let ps = self.level_ref(net);
        let Some(rte) = ps.rtable.get((v.varno - 1) as usize) else {
            return;
        };
        match &rte.kind {
            RteKind::Relation { relid, .. } => {
                t.resorigtbl = *relid;
                t.resorigcol = v.varattno;
            }
            RteKind::Subquery { subquery, .. } => {
                if v.varattno != 0 {
                    if let Some(ste) = subquery.target_list.iter().find(|s| s.resno == v.varattno) {
                        t.resorigtbl = ste.resorigtbl;
                        t.resorigcol = ste.resorigcol;
                    }
                }
            }
            RteKind::Cte {
                ctename,
                ctelevelsup,
                self_reference: false,
                ..
            } => {
                // GetCTEForRTE: the CTE lives `ctelevelsup` levels above
                // the level that holds the RTE.
                if v.varattno != 0 {
                    let up = net + *ctelevelsup as usize;
                    if up < self.depth() {
                        if let Some(cte) = self
                            .level_ref(up)
                            .ctenamespace
                            .iter()
                            .find(|c| &c.ctename == ctename)
                        {
                            if let Some(ste) = cte
                                .ctequery
                                .target_list
                                .iter()
                                .find(|s| s.resno == v.varattno)
                            {
                                t.resorigtbl = ste.resorigtbl;
                                t.resorigcol = ste.resorigcol;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `ExpandColumnRefStar`.
    fn expand_column_ref_star(
        &mut self,
        cref: &pb::ColumnRef,
        make_target_entry: bool,
    ) -> R<Vec<Starred>> {
        let fields: Vec<String> = cref
            .fields
            .iter()
            .filter_map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        if cref.fields.len() == 1 {
            // `ExpandAllTables`.
            let mut out = Vec::new();
            let mut found = false;
            let n = self.psr().namespace.len();
            for idx in 0..n {
                if !self.psr().namespace[idx].cols_visible {
                    continue;
                }
                found = true;
                out.extend(
                    self.expand_nsitem_attrs_at(idx, 0, true, cref.location)?
                        .into_iter()
                        .map(Starred::Tle),
                );
            }
            if !found {
                return unsupported("SELECT * with no tables specified is not valid");
            }
            return Ok(out);
        }
        let (nsp, rel) = match fields.as_slice() {
            [rel] => (None, rel.clone()),
            [nsp, rel] => (Some(nsp.clone()), rel.clone()),
            [cat, nsp, rel] => {
                if *cat != self.cat.data.database {
                    return unsupported("cross-database references are not implemented");
                }
                (Some(nsp.clone()), rel.clone())
            }
            _ => return unsupported("improper qualified name (too many dotted names)"),
        };
        let Some((up, idx)) = self.refname_namespace_item(nsp.as_deref(), &rel)? else {
            return unsupported(format!("missing FROM-clause entry for table \"{rel}\""));
        };
        self.expand_single_table(up, idx, cref.location, make_target_entry)
    }

    /// `ExpandSingleTable`.
    fn expand_single_table(
        &mut self,
        up: usize,
        idx: usize,
        location: i32,
        make_target_entry: bool,
    ) -> R<Vec<Starred>> {
        if make_target_entry {
            return Ok(self
                .expand_nsitem_attrs_at(idx, up, true, location)?
                .into_iter()
                .map(Starred::Tle)
                .collect());
        }
        let ns = self.level_ref(up).namespace[idx].clone();
        let (_, vars) = self.expand_nsitem_vars(&ns, up, location);
        {
            let rte = &mut self.level(up).rtable[(ns.rtindex - 1) as usize];
            if matches!(rte.kind, RteKind::Relation { .. }) {
                rte.required_perms |= 2;
            }
        }
        let mut out = Vec::new();
        for v in vars {
            self.mark_var_for_select_priv(&v);
            out.push(Starred::Expr(Node::Var(v)));
        }
        Ok(out)
    }

    /// `ExpandIndirectionStar`.
    fn expand_indirection_star(
        &mut self,
        ind: &pb::AIndirection,
        make_target_entry: bool,
        kind: ExprKind,
    ) -> R<Vec<Starred>> {
        let mut ind = ind.clone();
        ind.indirection.pop();
        let node = pb::Node {
            node: Some(N::AIndirection(Box::new(ind))),
        };
        let expr = self.transform_expr(&node, kind)?;
        self.expand_row_reference(expr, make_target_entry)
    }

    /// `ExpandRowReference`.
    fn expand_row_reference(&mut self, expr: Node, make_target_entry: bool) -> R<Vec<Starred>> {
        if let Node::Var(v) = &expr {
            if v.varattno == 0 {
                let up = v.varlevelsup as usize;
                let idx = self.nsitem_by_rtindex(v.varno, up)?;
                return self.expand_single_table(up, idx, v.location, make_target_entry);
            }
        }
        let tupdesc = match &expr {
            Node::Var(v) if v.vartype == RECORDOID => self.expand_record_variable(v, 0)?,
            _ => self.get_expr_result_tupdesc(&expr)?,
        };
        let mut out = Vec::new();
        for (i, a) in tupdesc.iter().enumerate() {
            if a.dropped {
                continue;
            }
            let fs = Node::FieldSelect(Box::new(FieldSelect {
                arg: expr.clone(),
                fieldnum: i as i32 + 1,
                resulttype: a.typid,
                resulttypmod: a.typmod,
                resultcollid: a.collation,
            }));
            if make_target_entry {
                let resno = self.ps().next_resno;
                self.ps().next_resno += 1;
                out.push(Starred::Tle(TargetEntry {
                    expr: fs,
                    resno,
                    resname: Some(a.name.clone()),
                    ressortgroupref: 0,
                    resorigtbl: 0,
                    resorigcol: 0,
                    resjunk: false,
                }));
            } else {
                out.push(Starred::Expr(fs));
            }
        }
        Ok(out)
    }

    /// `expandRecordVariable`.
    pub fn expand_record_variable(&self, v: &Var, levelsup: usize) -> R<Vec<TupDescAttr>> {
        let net = v.varlevelsup as usize + levelsup;
        if net >= self.depth() {
            return unsupported("a RECORD variable out of range");
        }
        let rte = self.level_ref(net).rtable[(v.varno - 1) as usize].clone();
        self.expand_record_in_rtable(&rte, v, net)
    }

    fn expand_record_in_rtable(
        &self,
        rte: &RangeTblEntry,
        v: &Var,
        net: usize,
    ) -> R<Vec<TupDescAttr>> {
        if v.varattno == 0 {
            let (names, vars) = self.expand_rte(rte, v.varno, 0, v.location)?;
            return Ok(names
                .into_iter()
                .zip(vars)
                .map(|(n, e)| TupDescAttr {
                    name: n,
                    typid: expr_type(self.cat, &e),
                    typmod: expr_typmod(self.cat, &e),
                    collation: expr_collation(self.cat, &e),
                    dropped: false,
                })
                .collect());
        }
        match &rte.kind {
            RteKind::Subquery { subquery, .. } => {
                let ste = subquery
                    .target_list
                    .iter()
                    .find(|t| t.resno == v.varattno && !t.resjunk)
                    .ok_or_else(|| AnalysisError("subquery attribute".into()))?;
                if let Node::Var(inner) = &ste.expr {
                    // Recurse into the subquery's own range table, whose
                    // parent is the level the subquery RTE belongs to.
                    let levelsup = inner.varlevelsup as usize;
                    if levelsup == 0 {
                        let r = subquery.rtable[(inner.varno - 1) as usize].clone();
                        return self.expand_record_in_rtable(&r, inner, net);
                    }
                    let lvl = net + levelsup - 1;
                    if lvl >= self.depth() {
                        return unsupported("a RECORD variable out of range");
                    }
                    let r = self.level_ref(lvl).rtable[(inner.varno - 1) as usize].clone();
                    return self.expand_record_in_rtable(&r, inner, lvl);
                }
                self.get_expr_result_tupdesc(&ste.expr)
            }
            RteKind::Join { joinaliasvars, .. } => {
                let e = joinaliasvars
                    .get((v.varattno - 1) as usize)
                    .ok_or_else(|| AnalysisError("join attribute".into()))?;
                if let Node::Var(inner) = e {
                    return self.expand_record_variable(inner, net);
                }
                self.get_expr_result_tupdesc(e)
            }
            _ => self.get_expr_result_tupdesc(&Node::Var(v.clone())),
        }
    }
}

/// One `*` expansion result.
pub(crate) enum Starred {
    Tle(TargetEntry),
    Expr(Node),
}

/// `FigureColname`.
pub(crate) fn figure_colname(node: &pb::Node) -> String {
    let mut name = None;
    figure_colname_internal(node, &mut name);
    name.unwrap_or_else(|| "?column?".to_string())
}

fn figure_colname_internal(node: &pb::Node, name: &mut Option<String>) -> i32 {
    let Some(n) = node.node.as_ref() else {
        return 0;
    };
    match n {
        N::ColumnRef(c) => {
            let mut fname = None;
            for f in &c.fields {
                if let Some(N::String(s)) = f.node.as_ref() {
                    fname = Some(s.sval.clone());
                }
            }
            if let Some(f) = fname {
                *name = Some(f);
                return 2;
            }
        }
        N::AIndirection(ind) => {
            let mut fname = None;
            for f in &ind.indirection {
                if let Some(N::String(s)) = f.node.as_ref() {
                    fname = Some(s.sval.clone());
                }
            }
            if let Some(f) = fname {
                *name = Some(f);
                return 2;
            }
            if let Some(arg) = ind.arg.as_deref() {
                return figure_colname_internal(arg, name);
            }
        }
        N::FuncCall(f) => {
            if let Some(N::String(s)) = f.funcname.last().and_then(|n| n.node.as_ref()) {
                *name = Some(s.sval.clone());
                return 2;
            }
        }
        N::AExpr(a) => {
            if a.kind == pb::AExprKind::AexprNullif as i32 {
                *name = Some("nullif".into());
                return 2;
            }
        }
        N::TypeCast(tc) => {
            let strength = tc
                .arg
                .as_deref()
                .map_or(0, |a| figure_colname_internal(a, name));
            if strength <= 1 {
                if let Some(tn) = &tc.type_name {
                    if let Some(N::String(s)) = tn.names.last().and_then(|n| n.node.as_ref()) {
                        *name = Some(s.sval.clone());
                        return 1;
                    }
                }
            }
            return strength;
        }
        N::CollateClause(c) => {
            if let Some(arg) = c.arg.as_deref() {
                return figure_colname_internal(arg, name);
            }
        }
        N::GroupingFunc(_) => {
            *name = Some("grouping".into());
            return 2;
        }
        N::SubLink(s) => match s.sub_link_type {
            x if x == pb::SubLinkType::ExistsSublink as i32 => {
                *name = Some("exists".into());
                return 2;
            }
            x if x == pb::SubLinkType::ArraySublink as i32 => {
                *name = Some("array".into());
                return 2;
            }
            x if x == pb::SubLinkType::ExprSublink as i32 => {
                // The first target's name in the subquery, as written.
                if let Some(N::SelectStmt(sel)) =
                    s.subselect.as_deref().and_then(|q| q.node.as_ref())
                {
                    if let Some(name2) = sublink_column_name(sel) {
                        *name = Some(name2);
                        return 2;
                    }
                }
            }
            _ => {}
        },
        N::CaseExpr(c) => {
            let strength = c
                .defresult
                .as_deref()
                .map_or(0, |d| figure_colname_internal(d, name));
            if strength <= 1 {
                *name = Some("case".into());
                return 1;
            }
            return strength;
        }
        N::AArrayExpr(_) => {
            *name = Some("array".into());
            return 2;
        }
        N::RowExpr(_) => {
            *name = Some("row".into());
            return 2;
        }
        N::CoalesceExpr(_) => {
            *name = Some("coalesce".into());
            return 2;
        }
        N::MinMaxExpr(m) => {
            *name = Some(
                if m.op == pb::MinMaxOp::IsGreatest as i32 {
                    "greatest"
                } else {
                    "least"
                }
                .into(),
            );
            return 2;
        }
        N::SqlvalueFunction(f) => {
            let n = match f.op - 1 {
                0 => "current_date",
                1 | 2 => "current_time",
                3 | 4 => "current_timestamp",
                5 | 6 => "localtime",
                7 | 8 => "localtimestamp",
                9 => "current_role",
                10 => "current_user",
                11 => "user",
                12 => "session_user",
                13 => "current_catalog",
                _ => "current_schema",
            };
            *name = Some(n.into());
            return 2;
        }
        N::XmlSerialize(_) => {
            *name = Some("xmlserialize".into());
            return 2;
        }
        _ => {}
    }
    0
}

/// The resname a SubLink's subquery gives its first column: transformSubLink
/// has run by the time FigureColname looks, so this is the analysed
/// target's name -- which for a raw SELECT is its alias or FigureColname of
/// its expression.
fn sublink_column_name(sel: &pb::SelectStmt) -> Option<String> {
    if sel.op != pb::SetOperation::SetopNone as i32 {
        let mut l = sel.larg.as_deref();
        while let Some(x) = l {
            if x.op == pb::SetOperation::SetopNone as i32 {
                return sublink_column_name(x);
            }
            l = x.larg.as_deref();
        }
        return None;
    }
    let first = sel.target_list.first()?;
    let Some(N::ResTarget(r)) = first.node.as_ref() else {
        return None;
    };
    if !r.name.is_empty() {
        return Some(r.name.clone());
    }
    let val = r.val.as_deref()?;
    if let Some(N::ColumnRef(c)) = val.node.as_ref() {
        if matches!(
            c.fields.last().and_then(|f| f.node.as_ref()),
            Some(N::AStar(_))
        ) {
            return None;
        }
    }
    Some(figure_colname(val))
}

/// `transformInsertStmt`.
pub(crate) fn transform_insert_stmt(a: &mut Analyzer<'_>, stmt: &pb::InsertStmt) -> R<Query> {
    let mut qry = Query {
        command_type: CMD_INSERT,
        ..Default::default()
    };
    a.ps().is_insert = true;
    if stmt.with_clause.is_some() {
        return unsupported("WITH in a stored rule");
    }
    let on_conflict = stmt.on_conflict_clause.as_deref();
    if on_conflict.is_some_and(|oc| oc.action != pb::OnConflictAction::OnconflictNothing as i32) {
        return unsupported("ON CONFLICT DO UPDATE in a stored rule");
    }
    qry.override_ = (stmt.r#override - 1).max(0);
    let select = stmt
        .select_stmt
        .as_deref()
        .and_then(|s| match s.node.as_ref() {
            Some(N::SelectStmt(x)) => Some(x.as_ref()),
            _ => None,
        });
    let is_general = select.is_some_and(|s| {
        s.values_lists.is_empty()
            || !s.sort_clause.is_empty()
            || s.limit_offset.is_some()
            || s.limit_count.is_some()
            || !s.locking_clause.is_empty()
            || s.with_clause.is_some()
    });
    let (sub_rtable, sub_namespace) = if is_general {
        (
            std::mem::take(&mut a.ps().rtable),
            std::mem::take(&mut a.ps().namespace),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let rel = stmt
        .relation
        .as_ref()
        .ok_or_else(|| AnalysisError("INSERT without a table".into()))?;
    qry.result_relation = a.set_target_table(rel, false, false, 1 /* ACL_INSERT */)?;
    let (icolumns, attrnos) = a.check_insert_targets(&stmt.cols)?;
    let target_rt = qry.result_relation;
    let mut expr_list: Vec<Node>;
    match select {
        None => expr_list = Vec::new(),
        Some(sel) if is_general => {
            let mut sub = super::parse::ParseState::new();
            sub.rtable = sub_rtable;
            sub.namespace = sub_namespace;
            sub.resolve_unknowns = false;
            a.levels.push(sub);
            let node = pb::Node {
                node: Some(N::SelectStmt(Box::new(sel.clone()))),
            };
            let r = a.transform_stmt(&node);
            a.levels.pop();
            let select_query = r?;
            let ns = a.add_rte_for_subquery(select_query, Alias::new("*SELECT*"), false, false)?;
            let rtindex = ns.rtindex;
            a.add_nsitem_to_query(ns, true, false, false);
            expr_list = Vec::new();
            let tl = match &a.psr().rtable[(rtindex - 1) as usize].kind {
                RteKind::Subquery { subquery, .. } => subquery.target_list.clone(),
                _ => unreachable!("a subquery"),
            };
            for t in tl {
                if t.resjunk {
                    continue;
                }
                let is_unknown_const = matches!(t.expr, Node::Const(_) | Node::Param(_))
                    && expr_type(a.cat, &t.expr) == UNKNOWNOID;
                if is_unknown_const {
                    expr_list.push(t.expr.clone());
                } else {
                    expr_list.push(Node::Var(Var {
                        varno: rtindex,
                        varattno: t.resno,
                        vartype: expr_type(a.cat, &t.expr),
                        vartypmod: expr_typmod(a.cat, &t.expr),
                        varcollid: expr_collation(a.cat, &t.expr),
                        varlevelsup: 0,
                        varnosyn: rtindex as u32,
                        varattnosyn: t.resno,
                        location: expr_location(&t.expr),
                    }));
                }
            }
            expr_list = a.transform_insert_row(expr_list, &stmt.cols, &icolumns, &attrnos)?;
        }
        Some(sel) if sel.values_lists.len() > 1 => {
            let mut lists = Vec::new();
            for row in &sel.values_lists {
                let Some(N::List(l)) = row.node.as_ref() else {
                    return unsupported("a VALUES row");
                };
                let sub = a.transform_expression_list(&l.items, ExprKind::Values, true)?;
                let mut sub = a.transform_insert_row(sub, &stmt.cols, &icolumns, &attrnos)?;
                for e in sub.iter_mut() {
                    a.assign_expr_collations(e)?;
                }
                lists.push(sub);
            }
            let first = lists.first().cloned().unwrap_or_default();
            let coltypes: Vec<Oid> = first.iter().map(|e| expr_type(a.cat, e)).collect();
            let coltypmods: Vec<i32> = first.iter().map(|e| expr_typmod(a.cat, e)).collect();
            let colcolls = vec![0; first.len()];
            let lateral = a.psr().rtable.len() != 1
                && lists
                    .iter()
                    .flatten()
                    .any(|e| super::nodefuncs::contain_vars_of_level(e, 0));
            let ns =
                a.add_rte_for_values(lists, coltypes, coltypmods, colcolls, None, lateral, true)?;
            let (_, vars) = a.expand_nsitem_vars(&ns, 0, -1);
            a.add_nsitem_to_query(ns, true, false, false);
            expr_list = vars.into_iter().map(Node::Var).collect();
            expr_list = a.transform_insert_row(expr_list, &stmt.cols, &icolumns, &attrnos)?;
        }
        Some(sel) => {
            let Some(N::List(l)) = sel.values_lists[0].node.as_ref() else {
                return unsupported("a VALUES row");
            };
            let items = l.items.clone();
            expr_list = a.transform_expression_list(&items, ExprKind::ValuesSingle, true)?;
            expr_list = a.transform_insert_row(expr_list, &stmt.cols, &icolumns, &attrnos)?;
        }
    }
    for ((expr, col), attno) in expr_list
        .into_iter()
        .zip(icolumns.iter())
        .zip(attrnos.iter())
    {
        qry.target_list.push(TargetEntry {
            expr,
            resno: *attno,
            resname: Some(col.clone()),
            ressortgroupref: 0,
            resorigtbl: 0,
            resorigcol: 0,
            resjunk: false,
        });
        a.ps().rtable[(target_rt - 1) as usize]
            .inserted_cols
            .insert(*attno - FIRST_LOW_INVALID);
    }
    // Clauses still to come see only the target relation.
    if on_conflict.is_some() || !stmt.returning_list.is_empty() {
        a.ps().namespace.clear();
        let ns = a.target_nsitem()?;
        a.add_nsitem_to_query(ns, false, true, true);
    }
    if let Some(oc) = on_conflict {
        qry.on_conflict = Some(a.transform_on_conflict_clause(oc)?);
    }
    if !stmt.returning_list.is_empty() {
        qry.returning_list = a.transform_returning_list(&stmt.returning_list)?;
    }
    let ps = a.ps();
    qry.rtable = std::mem::take(&mut ps.rtable);
    qry.jointree = Some(FromExpr {
        fromlist: std::mem::take(&mut ps.joinlist),
        quals: None,
    });
    qry.has_target_srfs = ps.has_target_srfs;
    qry.has_sub_links = ps.has_sub_links;
    a.assign_query_collations(&mut qry)?;
    Ok(qry)
}

/// `transformUpdateStmt`.
pub(crate) fn transform_update_stmt(a: &mut Analyzer<'_>, stmt: &pb::UpdateStmt) -> R<Query> {
    let mut qry = Query {
        command_type: CMD_UPDATE,
        ..Default::default()
    };
    if stmt.with_clause.is_some() {
        return unsupported("WITH in a stored rule");
    }
    let rel = stmt
        .relation
        .as_ref()
        .ok_or_else(|| AnalysisError("UPDATE without a table".into()))?;
    qry.result_relation = a.set_target_table(rel, rel.inh, true, 4 /* ACL_UPDATE */)?;
    let nsidx = a.psr().namespace.len() - 1;
    a.ps().namespace[nsidx].lateral_only = true;
    a.ps().namespace[nsidx].lateral_ok = false;
    a.transform_from_clause(&stmt.from_clause)?;
    a.ps().namespace[nsidx].lateral_only = false;
    a.ps().namespace[nsidx].lateral_ok = true;
    let qual = a.transform_where_clause(stmt.where_clause.as_deref(), ExprKind::Where)?;
    qry.returning_list = a.transform_returning_list(&stmt.returning_list)?;
    qry.target_list = a.transform_update_target_list(&stmt.target_list, qry.result_relation)?;
    let ps = a.ps();
    qry.rtable = std::mem::take(&mut ps.rtable);
    qry.jointree = Some(FromExpr {
        fromlist: std::mem::take(&mut ps.joinlist),
        quals: qual,
    });
    qry.has_target_srfs = ps.has_target_srfs;
    qry.has_sub_links = ps.has_sub_links;
    a.assign_query_collations(&mut qry)?;
    Ok(qry)
}

/// `transformDeleteStmt`.
pub(crate) fn transform_delete_stmt(a: &mut Analyzer<'_>, stmt: &pb::DeleteStmt) -> R<Query> {
    let mut qry = Query {
        command_type: CMD_DELETE,
        ..Default::default()
    };
    if stmt.with_clause.is_some() {
        return unsupported("WITH in a stored rule");
    }
    let rel = stmt
        .relation
        .as_ref()
        .ok_or_else(|| AnalysisError("DELETE without a table".into()))?;
    qry.result_relation = a.set_target_table(rel, rel.inh, true, 8 /* ACL_DELETE */)?;
    let nsidx = a.psr().namespace.len() - 1;
    a.ps().namespace[nsidx].lateral_only = true;
    a.ps().namespace[nsidx].lateral_ok = false;
    a.transform_from_clause(&stmt.using_clause)?;
    a.ps().namespace[nsidx].lateral_only = false;
    a.ps().namespace[nsidx].lateral_ok = true;
    let qual = a.transform_where_clause(stmt.where_clause.as_deref(), ExprKind::Where)?;
    qry.returning_list = a.transform_returning_list(&stmt.returning_list)?;
    let ps = a.ps();
    qry.rtable = std::mem::take(&mut ps.rtable);
    qry.jointree = Some(FromExpr {
        fromlist: std::mem::take(&mut ps.joinlist),
        quals: qual,
    });
    qry.has_sub_links = ps.has_sub_links;
    qry.has_window_funcs = ps.has_window_funcs;
    qry.has_target_srfs = ps.has_target_srfs;
    qry.has_aggs = ps.has_aggs;
    a.assign_query_collations(&mut qry)?;
    Ok(qry)
}

impl<'a> Analyzer<'a> {
    /// `setTargetTable`.
    fn set_target_table(
        &mut self,
        rv: &pb::RangeVar,
        inh: bool,
        also_source: bool,
        perms: u32,
    ) -> R<i32> {
        let relid = self.range_var_get_relid(rv)?;
        let alias = rv.alias.as_ref().map(super::relation::convert_alias);
        let ns = self.add_rte_for_relation_oid(relid, alias, 3, inh, false)?;
        let rtindex = ns.rtindex;
        self.ps().rtable[(rtindex - 1) as usize].required_perms = perms;
        self.ps().target_rtindex = Some(rtindex);
        if also_source {
            self.add_nsitem_to_query(ns, true, true, true);
        }
        Ok(rtindex)
    }

    fn target_nsitem(&self) -> R<super::parse::NsItem> {
        let rtindex = self
            .psr()
            .target_rtindex
            .ok_or_else(|| AnalysisError("no target relation".into()))?;
        let rte = &self.psr().rtable[(rtindex - 1) as usize];
        let relid = rte.relid().unwrap_or(0);
        let td = self.relation_tupdesc(relid);
        let cols = td
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if a.dropped {
                    super::parse::NsColumn::default()
                } else {
                    super::parse::NsColumn {
                        varno: rtindex,
                        varattno: i as i32 + 1,
                        vartype: a.typid,
                        vartypmod: a.typmod,
                        varcollid: a.collation,
                        varnosyn: rtindex,
                        varattnosyn: i as i32 + 1,
                        dontexpand: false,
                    }
                }
            })
            .collect();
        Ok(super::parse::NsItem {
            names: rte.eref.clone(),
            is_eref: true,
            rtindex,
            columns: cols,
            rel_visible: true,
            cols_visible: true,
            lateral_only: false,
            lateral_ok: true,
        })
    }

    /// `checkInsertTargets`: (column names, attnos).
    fn check_insert_targets(&self, cols: &[pb::Node]) -> R<(Vec<String>, Vec<i32>)> {
        let rtindex = self
            .psr()
            .target_rtindex
            .ok_or_else(|| AnalysisError("no target relation".into()))?;
        let relid = self.psr().rtable[(rtindex - 1) as usize]
            .relid()
            .unwrap_or(0);
        let td = self.relation_tupdesc(relid);
        if cols.is_empty() {
            let mut names = Vec::new();
            let mut nums = Vec::new();
            for (i, a) in td.iter().enumerate() {
                if a.dropped {
                    continue;
                }
                names.push(a.name.clone());
                nums.push(i as i32 + 1);
            }
            return Ok((names, nums));
        }
        let mut names = Vec::new();
        let mut nums = Vec::new();
        for c in cols {
            let Some(N::ResTarget(r)) = c.node.as_ref() else {
                return unsupported("an INSERT column");
            };
            if !r.indirection.is_empty() {
                return unsupported("INSERT into a subfield or element in a stored rule");
            }
            let attno = td
                .iter()
                .position(|a| !a.dropped && a.name == r.name)
                .ok_or_else(|| {
                    AnalysisError(format!("column \"{}\" of relation does not exist", r.name))
                })?;
            if nums.contains(&(attno as i32 + 1)) {
                return unsupported(format!("column \"{}\" specified more than once", r.name));
            }
            names.push(r.name.clone());
            nums.push(attno as i32 + 1);
        }
        Ok((names, nums))
    }

    /// `transformInsertRow`.
    fn transform_insert_row(
        &mut self,
        exprs: Vec<Node>,
        stmtcols: &[pb::Node],
        icolumns: &[String],
        attrnos: &[i32],
    ) -> R<Vec<Node>> {
        if exprs.len() > icolumns.len() {
            return unsupported("INSERT has more expressions than target columns");
        }
        if !stmtcols.is_empty() && exprs.len() < icolumns.len() {
            return unsupported("INSERT has more target columns than expressions");
        }
        let mut out = Vec::new();
        for ((e, col), attno) in exprs.into_iter().zip(icolumns).zip(attrnos) {
            out.push(self.transform_assigned_expr(e, ExprKind::InsertTarget, col, *attno)?);
        }
        Ok(out)
    }

    /// `transformAssignedExpr` (no indirection).
    fn transform_assigned_expr(
        &mut self,
        expr: Node,
        kind: ExprKind,
        colname: &str,
        attno: i32,
    ) -> R<Node> {
        let sv = self.ps().expr_kind;
        self.ps().expr_kind = kind;
        let rtindex = self.psr().target_rtindex.unwrap_or(0);
        let relid = self.psr().rtable[(rtindex - 1) as usize]
            .relid()
            .unwrap_or(0);
        let td = self.relation_tupdesc(relid);
        let att = td
            .get((attno - 1) as usize)
            .cloned()
            .ok_or_else(|| AnalysisError("a target column".into()))?;
        let expr = match expr {
            Node::SetToDefault(mut d) => {
                d.type_id = att.typid;
                d.type_mod = att.typmod;
                d.collation = att.collation;
                Node::SetToDefault(d)
            }
            e => e,
        };
        let t = expr_type(self.cat, &expr);
        let r = self.coerce_to_target_type(
            expr,
            t,
            att.typid,
            att.typmod,
            CoercionContext::Assignment,
            CoercionForm::ImplicitCast,
            -1,
        )?;
        self.ps().expr_kind = sv;
        r.ok_or_else(|| {
            AnalysisError(format!(
                "column \"{colname}\" is of a type the expression cannot be assigned to"
            ))
        })
    }

    /// `transformUpdateTargetList`.
    fn transform_update_target_list(
        &mut self,
        orig: &[pb::Node],
        rtindex: i32,
    ) -> R<Vec<TargetEntry>> {
        let mut tlist = self.transform_target_list(orig, ExprKind::UpdateSource)?;
        if self.psr().next_resno as usize > 1 {
            self.ps().next_resno = 1;
        }
        let relid = self.psr().rtable[(rtindex - 1) as usize]
            .relid()
            .unwrap_or(0);
        let td = self.relation_tupdesc(relid);
        for (t, o) in tlist.iter_mut().zip(orig) {
            let Some(N::ResTarget(r)) = o.node.as_ref() else {
                return unsupported("an UPDATE target");
            };
            if !r.indirection.is_empty() {
                return unsupported("UPDATE of a subfield or element in a stored rule");
            }
            let attno = td
                .iter()
                .position(|a| !a.dropped && a.name == r.name)
                .ok_or_else(|| AnalysisError(format!("column \"{}\" does not exist", r.name)))?
                as i32
                + 1;
            let e = t.expr.clone();
            t.expr = self.transform_assigned_expr(e, ExprKind::UpdateTarget, &r.name, attno)?;
            t.resno = attno;
            t.resname = Some(r.name.clone());
            self.ps().rtable[(rtindex - 1) as usize]
                .updated_cols
                .insert(attno - FIRST_LOW_INVALID);
        }
        Ok(tlist)
    }

    /// `transformReturningList`.
    /// `transformOnConflictClause` for DO NOTHING, with
    /// `transformOnConflictArbiter`'s inference columns and predicate.
    fn transform_on_conflict_clause(&mut self, oc: &pb::OnConflictClause) -> R<OnConflictExpr> {
        let mut arbiter_elems = Vec::new();
        let mut arbiter_where = None;
        if let Some(infer) = oc.infer.as_deref() {
            if !infer.conname.is_empty() {
                return unsupported("ON CONFLICT ON CONSTRAINT in a stored rule");
            }
            for ie in &infer.index_elems {
                let Some(N::IndexElem(e)) = ie.node.as_ref() else {
                    return unsupported("an ON CONFLICT inference element");
                };
                if e.ordering != pb::SortByDir::SortbyDefault as i32 {
                    return Err(AnalysisError(
                        "ASC/DESC is not allowed in ON CONFLICT clause".into(),
                    ));
                }
                if e.nulls_ordering != pb::SortByNulls::SortbyNullsDefault as i32 {
                    return Err(AnalysisError(
                        "NULLS FIRST/LAST is not allowed in ON CONFLICT clause".into(),
                    ));
                }
                if !e.collation.is_empty() || !e.opclass.is_empty() {
                    return unsupported(
                        "a collation or operator class in ON CONFLICT in a stored rule",
                    );
                }
                let parse = match e.expr.as_deref() {
                    Some(x) => x.clone(),
                    // A plain column: a ColumnRef at the inference clause.
                    None => pb::Node {
                        node: Some(N::ColumnRef(pb::ColumnRef {
                            fields: vec![pb::Node {
                                node: Some(N::String(pb::String {
                                    sval: e.name.clone(),
                                })),
                            }],
                            location: infer.location,
                        })),
                    },
                };
                let expr = self.transform_expr(&parse, ExprKind::IndexExpression)?;
                arbiter_elems.push(InferenceElem {
                    expr,
                    infercollid: 0,
                    inferopclass: 0,
                });
            }
            if let Some(w) = infer.where_clause.as_deref() {
                arbiter_where = Some(self.transform_expr(w, ExprKind::IndexPredicate)?);
            }
        }
        Ok(OnConflictExpr {
            action: oc.action - 1,
            arbiter_elems,
            arbiter_where,
            constraint: 0,
        })
    }

    fn transform_returning_list(&mut self, list: &[pb::Node]) -> R<Vec<TargetEntry>> {
        if list.is_empty() {
            return Ok(Vec::new());
        }
        let save = self.ps().next_resno;
        self.ps().next_resno = 1;
        let mut rlist = self.transform_target_list(list, ExprKind::Returning)?;
        self.mark_target_list_origins(&mut rlist);
        if self.psr().resolve_unknowns {
            self.resolve_target_list_unknowns(&mut rlist)?;
        }
        self.ps().next_resno = save;
        Ok(rlist)
    }
}
