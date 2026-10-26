//! `parse_relation.c`: range table entries, the namespace, and the Vars
//! that name their columns.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_type, expr_typmod};
use super::nodes::*;
use super::parse::{Analyzer, NsColumn, NsItem};
use super::{unsupported, AnalysisError, R};
use pg_query::protobuf as pb;

/// `FirstLowInvalidHeapAttributeNumber`.
pub(crate) const FIRST_LOW_INVALID: i32 = -7;

/// A system column: (name, attnum, type, typmod, collation).
const SYSTEM_COLUMNS: &[(&str, i32, Oid)] = &[
    ("ctid", -1, 27),
    ("xmin", -2, 28),
    ("cmin", -3, 29),
    ("xmax", -4, 28),
    ("cmax", -5, 29),
    ("tableoid", -6, OIDOID),
];

impl<'a> Analyzer<'a> {
    fn new_rte(alias: Option<Alias>, eref: Alias, kind: RteKind) -> RangeTblEntry {
        RangeTblEntry {
            alias,
            eref,
            kind,
            lateral: false,
            inh: false,
            in_from_cl: false,
            required_perms: 0,
            check_as_user: 0,
            selected_cols: Default::default(),
            inserted_cols: Default::default(),
            updated_cols: Default::default(),
        }
    }

    /// `buildRelationAliases`: the eref column names (and a rebuilt alias
    /// column list) for a relation's or function's tuple descriptor.
    fn build_relation_aliases(
        names: &[(String, bool)],
        alias: Option<&mut Alias>,
        eref: &mut Alias,
    ) -> R<()> {
        let mut given: std::collections::VecDeque<String> = Default::default();
        let mut rebuilt = Vec::new();
        let has_alias = alias.is_some();
        let numaliases;
        if let Some(a) = alias.as_ref() {
            given = a.colnames.iter().cloned().collect();
            numaliases = a.colnames.len();
        } else {
            numaliases = 0;
        }
        let mut numdropped = 0;
        for (name, dropped) in names {
            let attrname = if *dropped {
                numdropped += 1;
                if !given.is_empty() {
                    rebuilt.push(String::new());
                }
                String::new()
            } else if let Some(g) = given.pop_front() {
                rebuilt.push(g.clone());
                g
            } else {
                name.clone()
            };
            eref.colnames.push(attrname);
        }
        if !given.is_empty() {
            return unsupported(format!(
                "table \"{}\" has {} columns available but {} columns specified",
                eref.aliasname,
                names.len() - numdropped,
                numaliases
            ));
        }
        if let Some(a) = alias {
            if has_alias {
                a.colnames = rebuilt;
            }
        }
        Ok(())
    }

    fn nsitem_from_columns(rte: &RangeTblEntry, rtindex: i32, cols: &[NsColumn]) -> NsItem {
        NsItem {
            names: rte.eref.clone(),
            is_eref: true,
            rtindex,
            columns: cols.to_vec(),
            rel_visible: true,
            cols_visible: true,
            lateral_only: false,
            lateral_ok: true,
        }
    }

    /// The columns of a tuple descriptor as namespace columns.
    fn columns_of_tupdesc(rtindex: i32, atts: &[TupDescAttr]) -> Vec<NsColumn> {
        atts.iter()
            .enumerate()
            .map(|(i, a)| {
                if a.dropped {
                    NsColumn::default()
                } else {
                    NsColumn {
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
            .collect()
    }

    /// A relation's tuple descriptor.
    pub fn relation_tupdesc(&self, relid: Oid) -> Vec<TupDescAttr> {
        self.cat
            .rel_columns(relid)
            .into_iter()
            .map(|a| TupDescAttr {
                name: a.name.clone(),
                typid: a.typid,
                typmod: a.typmod,
                collation: a.collation,
                dropped: a.dropped,
            })
            .collect()
    }

    /// `addRangeTableEntryForRelation` (and `addRangeTableEntry`'s body):
    /// returns the namespace item, not yet in the namespace.
    pub fn add_rte_for_relation_oid(
        &mut self,
        relid: Oid,
        mut alias: Option<Alias>,
        lockmode: i32,
        inh: bool,
        in_from_cl: bool,
    ) -> R<NsItem> {
        let rel = self
            .cat
            .rel(relid)
            .ok_or_else(|| AnalysisError(format!("relation {relid} does not exist")))?
            .clone();
        let refname = alias
            .as_ref()
            .map_or(rel.name.clone(), |a| a.aliasname.clone());
        let tupdesc = self.relation_tupdesc(relid);
        let names: Vec<(String, bool)> = tupdesc
            .iter()
            .map(|a| (a.name.clone(), a.dropped))
            .collect();
        let mut eref = Alias::new(&refname);
        Self::build_relation_aliases(&names, alias.as_mut(), &mut eref)?;
        let mut rte = Self::new_rte(
            alias,
            eref,
            RteKind::Relation {
                relid,
                relkind: rel.kind,
                rellockmode: lockmode,
            },
        );
        rte.inh = inh;
        rte.in_from_cl = in_from_cl;
        rte.required_perms = 2;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        let cols = Self::columns_of_tupdesc(rtindex, &tupdesc);
        let rte = &self.psr().rtable[(rtindex - 1) as usize];
        Ok(Self::nsitem_from_columns(rte, rtindex, &cols))
    }

    /// `addRangeTableEntry` for a `RangeVar`.
    pub fn add_rte_for_rangevar(&mut self, rv: &pb::RangeVar, in_from_cl: bool) -> R<NsItem> {
        let alias = rv.alias.as_ref().map(convert_alias);
        let refname = alias
            .as_ref()
            .map_or(rv.relname.clone(), |a| a.aliasname.clone());
        let lockmode = if self.is_locked_refname(&refname) {
            2
        } else {
            1
        };
        let relid = self.range_var_get_relid(rv)?;
        self.add_rte_for_relation_oid(relid, alias, lockmode, rv.inh, in_from_cl)
    }

    fn is_locked_refname(&self, refname: &str) -> bool {
        let ps = self.psr();
        if ps.locked_from_parent {
            return true;
        }
        ps.locking_clause.iter().any(|lc| match lc.node.as_ref() {
            Some(pg_query::protobuf::node::Node::LockingClause(l)) => {
                l.locked_rels.is_empty()
                    || l.locked_rels.iter().any(|r| match r.node.as_ref() {
                        Some(pg_query::protobuf::node::Node::RangeVar(rv)) => rv.relname == refname,
                        _ => false,
                    })
            }
            _ => false,
        })
    }

    /// `RangeVarGetRelid`.
    pub fn range_var_get_relid(&self, rv: &pb::RangeVar) -> R<Oid> {
        if !rv.catalogname.is_empty() && rv.catalogname != self.cat.data.database {
            return unsupported("cross-database references are not implemented");
        }
        if !rv.schemaname.is_empty() {
            let nsp = self.cat.namespace_oid(&rv.schemaname).ok_or_else(|| {
                AnalysisError(format!("schema \"{}\" does not exist", rv.schemaname))
            })?;
            return self
                .cat
                .rels_named(&rv.relname)
                .find(|r| r.nsp == nsp)
                .map(|r| r.oid)
                .ok_or_else(|| {
                    AnalysisError(format!("relation \"{}\" does not exist", rv.relname))
                });
        }
        for nsp in &self.path {
            if let Some(r) = self.cat.rels_named(&rv.relname).find(|r| r.nsp == *nsp) {
                return Ok(r.oid);
            }
        }
        unsupported(format!("relation \"{}\" does not exist", rv.relname))
    }

    /// `addRangeTableEntryForSubquery`.
    pub fn add_rte_for_subquery(
        &mut self,
        subquery: Query,
        alias: Alias,
        lateral: bool,
        in_from_cl: bool,
    ) -> R<NsItem> {
        let mut eref = alias.clone();
        let numaliases = eref.colnames.len();
        let mut types = Vec::new();
        let mut varattno = 0;
        for te in &subquery.target_list {
            if te.resjunk {
                continue;
            }
            varattno += 1;
            if varattno > numaliases {
                eref.colnames.push(te.resname.clone().unwrap_or_default());
            }
            types.push((
                expr_type(self.cat, &te.expr),
                expr_typmod(self.cat, &te.expr),
                expr_collation(self.cat, &te.expr),
            ));
        }
        if varattno < numaliases {
            return unsupported(format!(
                "table \"{}\" has {varattno} columns available but {numaliases} columns specified",
                alias.aliasname
            ));
        }
        let mut rte = Self::new_rte(
            Some(alias),
            eref,
            RteKind::Subquery {
                subquery: Box::new(subquery),
                security_barrier: false,
            },
        );
        rte.lateral = lateral;
        rte.in_from_cl = in_from_cl;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        Ok(self.nsitem_from_lists(rtindex, &types))
    }

    /// `addRangeTableEntryForCTE` for a CTE whose analysis is complete (a
    /// self-reference needs WITH RECURSIVE, which is not transcribed).
    pub fn add_rte_for_cte(
        &mut self,
        levelsup: usize,
        idx: usize,
        rv: &pb::RangeVar,
        in_from_cl: bool,
    ) -> R<NsItem> {
        let alias = rv.alias.as_ref().map(convert_alias);
        let cte = &mut self.level(levelsup).ctenamespace[idx];
        // Self-reference if and only if the CTE's analysis is not complete;
        // only another reference bumps its refcount.
        let self_reference = !cte.analyzed;
        if !self_reference {
            cte.cterefcount += 1;
        }
        let cte = cte.clone();
        let refname = alias
            .as_ref()
            .map_or_else(|| cte.ctename.clone(), |a| a.aliasname.clone());
        let mut eref = alias.clone().unwrap_or_else(|| Alias::new(&refname));
        let numaliases = eref.colnames.len();
        let mut varattno = 0;
        for n in &cte.ctecolnames {
            varattno += 1;
            if varattno > numaliases {
                eref.colnames.push(n.clone());
            }
        }
        if varattno < numaliases {
            return Err(AnalysisError(format!(
                "table \"{refname}\" has {varattno} columns available but {numaliases} columns specified"
            )));
        }
        let mut rte = Self::new_rte(
            alias,
            eref,
            RteKind::Cte {
                ctename: cte.ctename.clone(),
                ctelevelsup: levelsup as u32,
                self_reference,
                coltypes: cte.ctecoltypes.clone(),
                coltypmods: cte.ctecoltypmods.clone(),
                colcollations: cte.ctecolcollations.clone(),
            },
        );
        rte.in_from_cl = in_from_cl;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        let types: Vec<(Oid, i32, Oid)> = (0..cte.ctecoltypes.len())
            .map(|i| {
                (
                    cte.ctecoltypes[i],
                    cte.ctecoltypmods[i],
                    cte.ctecolcollations[i],
                )
            })
            .collect();
        Ok(self.nsitem_from_lists(rtindex, &types))
    }

    fn nsitem_from_lists(&self, rtindex: i32, types: &[(Oid, i32, Oid)]) -> NsItem {
        let cols: Vec<NsColumn> = types
            .iter()
            .enumerate()
            .map(|(i, (t, m, c))| NsColumn {
                varno: rtindex,
                varattno: i as i32 + 1,
                vartype: *t,
                vartypmod: *m,
                varcollid: *c,
                varnosyn: rtindex,
                varattnosyn: i as i32 + 1,
                dontexpand: false,
            })
            .collect();
        let rte = &self.psr().rtable[(rtindex - 1) as usize];
        Self::nsitem_from_columns(rte, rtindex, &cols)
    }

    /// `addRangeTableEntryForFunction`.
    pub fn add_rte_for_function(
        &mut self,
        funcnames: Vec<String>,
        funcexprs: Vec<Node>,
        coldeflists: Vec<Vec<pb::Node>>,
        rangefunc: &pb::RangeFunction,
        lateral: bool,
        in_from_cl: bool,
    ) -> R<NsItem> {
        let mut alias = rangefunc.alias.as_ref().map(convert_alias);
        let aliasname = match &alias {
            Some(a) => a.aliasname.clone(),
            None => funcnames.first().cloned().unwrap_or_default(),
        };
        let mut eref = Alias::new(&aliasname);
        let nfuncs = funcexprs.len();
        let mut functions = Vec::new();
        let mut tupdescs: Vec<Vec<TupDescAttr>> = Vec::new();
        for ((funcexpr, funcname), coldeflist) in funcexprs
            .into_iter()
            .zip(funcnames.iter())
            .zip(coldeflists.iter())
        {
            let mut rtfunc = RangeTblFunction {
                funcexpr: funcexpr.clone(),
                funccolcount: 0,
                funccolnames: Vec::new(),
                funccoltypes: Vec::new(),
                funccoltypmods: Vec::new(),
                funccolcollations: Vec::new(),
            };
            let (class, rettype, tupdesc) = self.get_expr_result_type(&funcexpr)?;
            use super::funcapi::TypeFuncClass as C;
            if !coldeflist.is_empty() && class != C::Record {
                return unsupported("a column definition list here");
            }
            if coldeflist.is_empty() && class == C::Record {
                return unsupported(
                    "a column definition list is required for functions returning \"record\"",
                );
            }
            let td = match class {
                C::Composite | C::CompositeDomain => tupdesc.unwrap_or_default(),
                C::Scalar => {
                    let name = self.choose_scalar_function_alias(
                        &funcexpr,
                        funcname,
                        alias.as_ref(),
                        nfuncs,
                    );
                    vec![TupDescAttr {
                        name,
                        typid: rettype,
                        typmod: expr_typmod(self.cat, &funcexpr),
                        collation: expr_collation(self.cat, &funcexpr),
                        dropped: false,
                    }]
                }
                C::Record => {
                    let mut td = Vec::new();
                    for c in coldeflist {
                        let Some(pg_query::protobuf::node::Node::ColumnDef(cd)) = c.node.as_ref()
                        else {
                            return unsupported("a column definition");
                        };
                        let tn = cd
                            .type_name
                            .as_ref()
                            .ok_or_else(|| AnalysisError("a column without a type".into()))?;
                        let (t, m) = self.typename_type_id_and_mod(tn)?;
                        let coll = self.column_def_collation(cd, t)?;
                        rtfunc.funccolnames.push(cd.colname.clone());
                        rtfunc.funccoltypes.push(t);
                        rtfunc.funccoltypmods.push(m);
                        rtfunc.funccolcollations.push(coll);
                        td.push(TupDescAttr {
                            name: cd.colname.clone(),
                            typid: t,
                            typmod: m,
                            collation: coll,
                            dropped: false,
                        });
                    }
                    td
                }
                C::Other => {
                    return unsupported(format!(
                        "function \"{funcname}\" in FROM has unsupported return type"
                    ))
                }
            };
            rtfunc.funccolcount = td.len() as i32;
            functions.push(rtfunc);
            tupdescs.push(td);
        }
        let mut tupdesc: Vec<TupDescAttr> = if nfuncs > 1 || rangefunc.ordinality {
            tupdescs.iter().flatten().cloned().collect()
        } else {
            tupdescs.pop().unwrap_or_default()
        };
        if rangefunc.ordinality {
            tupdesc.push(TupDescAttr {
                name: "ordinality".into(),
                typid: INT8OID,
                typmod: -1,
                collation: 0,
                dropped: false,
            });
        }
        let names: Vec<(String, bool)> = tupdesc
            .iter()
            .map(|a| (a.name.clone(), a.dropped))
            .collect();
        Self::build_relation_aliases(&names, alias.as_mut(), &mut eref)?;
        let mut rte = Self::new_rte(
            alias,
            eref,
            RteKind::Function {
                functions,
                funcordinality: rangefunc.ordinality,
            },
        );
        rte.lateral = lateral;
        rte.in_from_cl = in_from_cl;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        let cols = Self::columns_of_tupdesc(rtindex, &tupdesc);
        let rte = &self.psr().rtable[(rtindex - 1) as usize];
        Ok(Self::nsitem_from_columns(rte, rtindex, &cols))
    }

    /// `chooseScalarFunctionAlias`.
    fn choose_scalar_function_alias(
        &self,
        funcexpr: &Node,
        funcname: &str,
        alias: Option<&Alias>,
        nfuncs: usize,
    ) -> String {
        if let Node::FuncExpr(f) = funcexpr {
            if let Some(n) = self.func_result_name(f.funcid) {
                return n;
            }
        }
        if nfuncs == 1 {
            if let Some(a) = alias {
                return a.aliasname.clone();
            }
        }
        funcname.to_string()
    }

    /// `get_func_result_name`: the name of a function's single OUT
    /// parameter.
    fn func_result_name(&self, f: Oid) -> Option<String> {
        let p = self.cat.proc_row(f)?;
        let modes = p.argmodes.as_ref()?;
        let names = p.argnames.as_ref()?;
        let mut result = None;
        let mut nout = 0;
        for (i, m) in modes.iter().enumerate() {
            if matches!(m, 'o' | 'b' | 't') {
                nout += 1;
                let n = names.get(i).cloned().unwrap_or_default();
                if n.is_empty() {
                    return None;
                }
                result = Some(n);
            }
        }
        (nout == 1).then_some(result).flatten()
    }

    /// `addRangeTableEntryForValues`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_rte_for_values(
        &mut self,
        exprs: Vec<Vec<Node>>,
        coltypes: Vec<Oid>,
        coltypmods: Vec<i32>,
        colcollations: Vec<Oid>,
        alias: Option<Alias>,
        lateral: bool,
        in_from_cl: bool,
    ) -> R<NsItem> {
        let refname = alias
            .as_ref()
            .map_or("*VALUES*".to_string(), |a| a.aliasname.clone());
        let mut eref = alias.clone().unwrap_or_else(|| Alias::new(&refname));
        let numcolumns = exprs.first().map_or(0, Vec::len);
        let mut numaliases = eref.colnames.len();
        while numaliases < numcolumns {
            numaliases += 1;
            eref.colnames.push(format!("column{numaliases}"));
        }
        if numcolumns < numaliases {
            return unsupported("VALUES lists have fewer columns than aliases");
        }
        let types: Vec<(Oid, i32, Oid)> = coltypes
            .iter()
            .zip(coltypmods.iter())
            .zip(colcollations.iter())
            .map(|((t, m), c)| (*t, *m, *c))
            .collect();
        let mut rte = Self::new_rte(
            alias,
            eref,
            RteKind::Values {
                values_lists: exprs,
                coltypes,
                coltypmods,
                colcollations,
            },
        );
        rte.lateral = lateral;
        rte.in_from_cl = in_from_cl;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        Ok(self.nsitem_from_lists(rtindex, &types))
    }

    /// `addRangeTableEntryForJoin`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_rte_for_join(
        &mut self,
        colnames: Vec<String>,
        nscolumns: Vec<NsColumn>,
        jointype: i32,
        nummergedcols: i32,
        aliasvars: Vec<Node>,
        leftcols: Vec<i32>,
        rightcols: Vec<i32>,
        join_using_alias: Option<Alias>,
        alias: Option<Alias>,
        in_from_cl: bool,
    ) -> NsItem {
        let mut eref = alias.clone().unwrap_or_else(|| Alias::new("unnamed_join"));
        let numaliases = eref.colnames.len();
        if numaliases < colnames.len() {
            eref.colnames.extend(colnames[numaliases..].iter().cloned());
        }
        let mut rte = Self::new_rte(
            alias,
            eref,
            RteKind::Join {
                jointype,
                joinmergedcols: nummergedcols,
                joinaliasvars: aliasvars,
                joinleftcols: leftcols,
                joinrightcols: rightcols,
                join_using_alias,
            },
        );
        rte.in_from_cl = in_from_cl;
        self.ps().rtable.push(rte);
        let rtindex = self.psr().rtable.len() as i32;
        let rte = &self.psr().rtable[(rtindex - 1) as usize];
        Self::nsitem_from_columns(rte, rtindex, &nscolumns)
    }

    /// `addNSItemToQuery`.
    pub fn add_nsitem_to_query(
        &mut self,
        mut ns: NsItem,
        to_joinlist: bool,
        to_rel: bool,
        to_var: bool,
    ) {
        if to_joinlist {
            self.ps()
                .joinlist
                .push(JoinTreeNode::RangeTblRef(ns.rtindex));
        }
        if to_rel || to_var {
            ns.rel_visible = to_rel;
            ns.cols_visible = to_var;
            ns.lateral_only = false;
            ns.lateral_ok = true;
            self.ps().namespace.push(ns);
        }
    }

    /// `markRTEForSelectPriv`.
    fn mark_rte_for_select_priv(&mut self, up: usize, rtindex: i32, col: i32) {
        let ps = self.level(up);
        let Some(rte) = ps.rtable.get_mut((rtindex - 1) as usize) else {
            return;
        };
        match &rte.kind {
            RteKind::Relation { .. } => {
                rte.required_perms |= 2;
                rte.selected_cols.insert(col - FIRST_LOW_INVALID);
            }
            RteKind::Join { .. } => {
                if col == 0 {
                    let Some(Some((l, r))) = ps.joinexprs.get((rtindex - 1) as usize).cloned()
                    else {
                        return;
                    };
                    for side in [l, r] {
                        let idx = match side {
                            JoinTreeNode::RangeTblRef(i) => i,
                            JoinTreeNode::JoinExpr(j) => j.rtindex,
                        };
                        self.mark_rte_for_select_priv(up, idx, 0);
                    }
                }
            }
            _ => {}
        }
    }

    /// `markVarForSelectPriv`.
    pub fn mark_var_for_select_priv(&mut self, v: &Var) {
        self.mark_rte_for_select_priv(v.varlevelsup as usize, v.varno, v.varattno);
    }

    /// `scanNSItemForColumn`: the namespace item at `idx` of level `up`.
    pub fn scan_nsitem_for_column(
        &mut self,
        up: usize,
        idx: usize,
        colname: &str,
        location: i32,
    ) -> R<Option<Node>> {
        let ns = self.level_ref(up).namespace[idx].clone();
        let attnum = self.scan_rte_for_column(up, &ns, colname)?;
        let Some(attnum) = attnum else {
            return Ok(None);
        };
        let var = if attnum > 0 {
            let c = &ns.columns[(attnum - 1) as usize];
            if c.varno == 0 {
                return unsupported(format!(
                    "column \"{colname}\" of relation \"{}\" does not exist",
                    ns.names.aliasname
                ));
            }
            Var {
                varno: c.varno,
                varattno: c.varattno,
                vartype: c.vartype,
                vartypmod: c.vartypmod,
                varcollid: c.varcollid,
                varlevelsup: up as u32,
                varnosyn: c.varnosyn as u32,
                varattnosyn: c.varattnosyn,
                location,
            }
        } else {
            let (_, _, t) = SYSTEM_COLUMNS
                .iter()
                .find(|(_, n, _)| *n == attnum)
                .copied()
                .ok_or_else(|| AnalysisError("a system column".into()))?;
            Var {
                varno: ns.rtindex,
                varattno: attnum,
                vartype: t,
                vartypmod: -1,
                varcollid: 0,
                varlevelsup: up as u32,
                varnosyn: ns.rtindex as u32,
                varattnosyn: attnum,
                location,
            }
        };
        self.mark_var_for_select_priv(&var);
        Ok(Some(Node::Var(var)))
    }

    /// `scanRTEForColumn`.
    fn scan_rte_for_column(&self, up: usize, ns: &NsItem, colname: &str) -> R<Option<i32>> {
        let mut result = None;
        for (i, c) in ns.names.colnames.iter().enumerate() {
            if c == colname {
                if result.is_some() {
                    return unsupported(format!("column reference \"{colname}\" is ambiguous"));
                }
                result = Some(i as i32 + 1);
            }
        }
        if result.is_some() {
            return Ok(result);
        }
        let rte = &self.level_ref(up).rtable[(ns.rtindex - 1) as usize];
        if let RteKind::Relation { relid, relkind, .. } = &rte.kind {
            if *relkind != 'c' {
                if let Some((_, n, _)) = SYSTEM_COLUMNS.iter().find(|(name, _, _)| *name == colname)
                {
                    // `SearchSysCacheExists2(ATTNUM, relid, attnum)`: a view
                    // has no system columns.
                    if self.cat.system_attribute(*relid, colname).is_some()
                        || matches!(*relkind, 'r' | 'p' | 'm' | 't' | 'f')
                    {
                        return Ok(Some(*n));
                    }
                }
            }
        }
        Ok(None)
    }

    /// `colNameToVar`.
    pub fn col_name_to_var(
        &mut self,
        colname: &str,
        localonly: bool,
        location: i32,
    ) -> R<Option<Node>> {
        let mut up = 0;
        while up < self.depth() {
            let mut result = None;
            let lateral_active = self.level_ref(up).lateral_active;
            let n = self.level_ref(up).namespace.len();
            for idx in 0..n {
                let ns = &self.level_ref(up).namespace[idx];
                if !ns.cols_visible {
                    continue;
                }
                if ns.lateral_only && !lateral_active {
                    continue;
                }
                if let Some(v) = self.scan_nsitem_for_column(up, idx, colname, location)? {
                    if result.is_some() {
                        return unsupported(format!("column reference \"{colname}\" is ambiguous"));
                    }
                    let ns = &self.level_ref(up).namespace[idx];
                    if ns.lateral_only && !ns.lateral_ok {
                        return unsupported("invalid reference to FROM-clause entry");
                    }
                    result = Some(v);
                }
            }
            if result.is_some() || localonly {
                return Ok(result);
            }
            up += 1;
        }
        Ok(None)
    }

    /// `refnameNamespaceItem`: (levels up, index in that level's
    /// namespace).
    pub fn refname_namespace_item(
        &self,
        schemaname: Option<&str>,
        refname: &str,
    ) -> R<Option<(usize, usize)>> {
        let mut relid = None;
        if let Some(s) = schemaname {
            let Some(nsp) = self.cat.namespace_oid(s) else {
                return Ok(None);
            };
            let Some(r) = self.cat.rels_named(refname).find(|r| r.nsp == nsp) else {
                return Ok(None);
            };
            relid = Some(r.oid);
        }
        for up in 0..self.depth() {
            let ps = self.level_ref(up);
            let mut result = None;
            for (idx, ns) in ps.namespace.iter().enumerate() {
                if !ns.rel_visible {
                    continue;
                }
                if ns.lateral_only && !ps.lateral_active {
                    continue;
                }
                let matched = match relid {
                    Some(relid) => {
                        let rte = &ps.rtable[(ns.rtindex - 1) as usize];
                        rte.relid() == Some(relid) && rte.alias.is_none()
                    }
                    None => ns.names.aliasname == refname,
                };
                if matched {
                    if result.is_some() {
                        return unsupported(format!("table reference \"{refname}\" is ambiguous"));
                    }
                    if ns.lateral_only && !ns.lateral_ok {
                        return unsupported("invalid reference to FROM-clause entry");
                    }
                    result = Some((up, idx));
                }
            }
            if result.is_some() {
                return Ok(result);
            }
        }
        Ok(None)
    }

    /// `expandNSItemVars`: (names, vars).
    pub fn expand_nsitem_vars(
        &self,
        ns: &NsItem,
        sublevels_up: usize,
        location: i32,
    ) -> (Vec<String>, Vec<Var>) {
        let mut names = Vec::new();
        let mut vars = Vec::new();
        for (i, name) in ns.names.colnames.iter().enumerate() {
            let c = &ns.columns[i];
            if c.dontexpand {
                continue;
            }
            if name.is_empty() {
                continue;
            }
            vars.push(Var {
                varno: c.varno,
                varattno: c.varattno,
                vartype: c.vartype,
                vartypmod: c.vartypmod,
                varcollid: c.varcollid,
                varlevelsup: sublevels_up as u32,
                varnosyn: c.varnosyn as u32,
                varattnosyn: c.varattnosyn,
                location,
            });
            names.push(name.clone());
        }
        (names, vars)
    }

    /// `expandNSItemAttrs` for the namespace item at `idx` of `up`.
    pub fn expand_nsitem_attrs_at(
        &mut self,
        idx: usize,
        up: usize,
        require_col_privs: bool,
        location: i32,
    ) -> R<Vec<TargetEntry>> {
        let ns = self.level_ref(up).namespace[idx].clone();
        self.expand_nsitem_attrs(&ns, up, require_col_privs, location)
    }

    pub fn expand_nsitem_attrs(
        &mut self,
        ns: &NsItem,
        up: usize,
        require_col_privs: bool,
        location: i32,
    ) -> R<Vec<TargetEntry>> {
        let (names, vars) = self.expand_nsitem_vars(ns, up, location);
        {
            let rte = &mut self.level(up).rtable[(ns.rtindex - 1) as usize];
            if matches!(rte.kind, RteKind::Relation { .. }) {
                rte.required_perms |= 2;
            }
        }
        let mut out = Vec::new();
        for (name, var) in names.into_iter().zip(vars) {
            let resno = self.ps().next_resno;
            self.ps().next_resno += 1;
            if require_col_privs {
                self.mark_var_for_select_priv(&var);
            }
            out.push(TargetEntry {
                expr: Node::Var(var),
                resno,
                resname: Some(name),
                ressortgroupref: 0,
                resorigtbl: 0,
                resorigcol: 0,
                resjunk: false,
            });
        }
        Ok(out)
    }

    /// `expandRTE` (names, vars; dropped columns omitted) for an RTE of the
    /// level `up`.
    pub fn expand_rte(
        &self,
        rte: &RangeTblEntry,
        rtindex: i32,
        sublevels_up: usize,
        location: i32,
    ) -> R<(Vec<String>, Vec<Node>)> {
        let mut names = Vec::new();
        let mut vars = Vec::new();
        let mk = |attno: i32, t: Oid, m: i32, c: Oid| {
            Node::Var(Var {
                varno: rtindex,
                varattno: attno,
                vartype: t,
                vartypmod: m,
                varcollid: c,
                varlevelsup: sublevels_up as u32,
                varnosyn: rtindex as u32,
                varattnosyn: attno,
                location,
            })
        };
        match &rte.kind {
            RteKind::Relation { relid, .. } => {
                let td = self.relation_tupdesc(*relid);
                for (i, a) in td.iter().enumerate() {
                    if a.dropped {
                        continue;
                    }
                    names.push(
                        rte.eref
                            .colnames
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| a.name.clone()),
                    );
                    vars.push(mk(i as i32 + 1, a.typid, a.typmod, a.collation));
                }
            }
            RteKind::Subquery { subquery, .. } => {
                let mut attno = 0;
                for te in &subquery.target_list {
                    if te.resjunk {
                        continue;
                    }
                    attno += 1;
                    let Some(label) = rte.eref.colnames.get((attno - 1) as usize) else {
                        break;
                    };
                    names.push(label.clone());
                    vars.push(mk(
                        attno,
                        expr_type(self.cat, &te.expr),
                        expr_typmod(self.cat, &te.expr),
                        expr_collation(self.cat, &te.expr),
                    ));
                }
            }
            RteKind::Function {
                functions,
                funcordinality,
            } => {
                let mut done = 0;
                for f in functions {
                    if !f.funccolnames.is_empty() {
                        for i in 0..f.funccolcount as usize {
                            names.push(rte.eref.colnames[done + i].clone());
                            vars.push(mk(
                                (done + i) as i32 + 1,
                                f.funccoltypes[i],
                                f.funccoltypmods[i],
                                f.funccolcollations[i],
                            ));
                        }
                    } else {
                        let (class, rettype, td) = self.get_expr_result_type(&f.funcexpr)?;
                        use super::funcapi::TypeFuncClass as C;
                        match class {
                            C::Composite | C::CompositeDomain => {
                                for (i, a) in td.unwrap_or_default().iter().enumerate() {
                                    if i >= f.funccolcount as usize {
                                        break;
                                    }
                                    if a.dropped {
                                        continue;
                                    }
                                    names.push(
                                        rte.eref
                                            .colnames
                                            .get(done + i)
                                            .cloned()
                                            .unwrap_or_else(|| a.name.clone()),
                                    );
                                    vars.push(mk(
                                        (done + i) as i32 + 1,
                                        a.typid,
                                        a.typmod,
                                        a.collation,
                                    ));
                                }
                            }
                            C::Scalar => {
                                names.push(rte.eref.colnames[done].clone());
                                vars.push(mk(
                                    done as i32 + 1,
                                    rettype,
                                    expr_typmod(self.cat, &f.funcexpr),
                                    expr_collation(self.cat, &f.funcexpr),
                                ));
                            }
                            _ => {
                                return unsupported("function in FROM has unsupported return type")
                            }
                        }
                    }
                    done += f.funccolcount as usize;
                }
                if *funcordinality {
                    names.push(rte.eref.colnames.last().cloned().unwrap_or_default());
                    vars.push(Node::Var(Var {
                        varno: rtindex,
                        varattno: done as i32 + 1,
                        vartype: INT8OID,
                        vartypmod: -1,
                        varcollid: 0,
                        varlevelsup: sublevels_up as u32,
                        varnosyn: rtindex as u32,
                        varattnosyn: done as i32 + 1,
                        location: -1,
                    }));
                }
            }
            RteKind::Join { joinaliasvars, .. } => {
                for (i, (name, avar)) in rte.eref.colnames.iter().zip(joinaliasvars).enumerate() {
                    names.push(name.clone());
                    let v = match avar {
                        Node::Var(v) => {
                            let mut v = v.clone();
                            v.varlevelsup = sublevels_up as u32;
                            v.location = location;
                            Node::Var(v)
                        }
                        other => mk(
                            i as i32 + 1,
                            expr_type(self.cat, other),
                            expr_typmod(self.cat, other),
                            expr_collation(self.cat, other),
                        ),
                    };
                    let v = match v {
                        Node::Var(mut v) => {
                            v.location = location;
                            Node::Var(v)
                        }
                        o => o,
                    };
                    vars.push(v);
                }
            }
            RteKind::Values {
                coltypes,
                coltypmods,
                colcollations,
                ..
            }
            | RteKind::Cte {
                coltypes,
                coltypmods,
                colcollations,
                ..
            } => {
                for i in 0..coltypes.len() {
                    if coltypes[i] == 0 {
                        continue;
                    }
                    names.push(rte.eref.colnames[i].clone());
                    vars.push(mk(
                        i as i32 + 1,
                        coltypes[i],
                        coltypmods[i],
                        colcollations[i],
                    ));
                }
            }
            RteKind::Result => {}
        }
        Ok((names, vars))
    }

    /// `GetNSItemByRangeTablePosn`: the index of the namespace item for
    /// `varno` at level `up`.
    pub fn nsitem_by_rtindex(&self, varno: i32, up: usize) -> R<usize> {
        self.level_ref(up)
            .namespace
            .iter()
            .position(|n| n.rtindex == varno)
            .ok_or_else(|| AnalysisError("nsitem not found".into()))
    }

    /// `ColumnDef` collation: the type's, or an explicit COLLATE.
    fn column_def_collation(&self, cd: &pb::ColumnDef, t: Oid) -> R<Oid> {
        if let Some(c) = &cd.coll_clause {
            return self.lookup_collation(&c.collname);
        }
        Ok(self.cat.get_typcollation(t))
    }
}

/// A tuple descriptor's attribute.
#[derive(Clone, Debug, Default)]
pub(crate) struct TupDescAttr {
    pub name: String,
    pub typid: Oid,
    pub typmod: i32,
    pub collation: Oid,
    pub dropped: bool,
}

pub(crate) fn convert_alias(a: &pb::Alias) -> Alias {
    Alias {
        aliasname: a.aliasname.clone(),
        colnames: a
            .colnames
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(pg_query::protobuf::node::Node::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect(),
    }
}
