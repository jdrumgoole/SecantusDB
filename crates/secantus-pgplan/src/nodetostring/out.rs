//! `nodeToString`: PostgreSQL 15.19's `outfuncs.c`, for the nodes a stored
//! rule holds. Every writer here is a transcription of its `_outXxx`
//! routine, field for field and in its order.

use super::nodes::*;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// The printer. In `equal` mode, locations and coercion display forms are
/// written as constants, so two trees print alike exactly when
/// `equalfuncs.c`'s `equal()` holds them equal (it ignores both).
pub(crate) struct Out {
    pub s: String,
    equal: bool,
}

impl Out {
    pub fn new() -> Self {
        Out {
            s: String::new(),
            equal: false,
        }
    }

    pub fn for_equal() -> Self {
        Out {
            s: String::new(),
            equal: true,
        }
    }

    fn label(&mut self, l: &str) {
        self.s.push_str(l);
    }

    fn int(&mut self, f: &str, v: i64) {
        let _ = write!(self.s, " :{f} {v}");
    }

    fn uint(&mut self, f: &str, v: u32) {
        let _ = write!(self.s, " :{f} {v}");
    }

    fn boolean(&mut self, f: &str, v: bool) {
        let _ = write!(self.s, " :{f} {}", if v { "true" } else { "false" });
    }

    fn loc(&mut self, f: &str, v: i32) {
        let v = if self.equal { 0 } else { v };
        let _ = write!(self.s, " :{f} {v}");
    }

    fn form(&mut self, f: &str, v: CoercionForm) {
        let v = if self.equal { 0 } else { v as i32 };
        let _ = write!(self.s, " :{f} {v}");
    }

    fn string(&mut self, f: &str, v: Option<&str>) {
        let _ = write!(self.s, " :{f} ");
        out_token(&mut self.s, v);
    }

    fn chr(&mut self, f: &str, c: char) {
        let _ = write!(self.s, " :{f} ");
        let mut buf = [0u8; 4];
        out_token(&mut self.s, Some(c.encode_utf8(&mut buf)));
    }

    fn field(&mut self, f: &str) {
        let _ = write!(self.s, " :{f} ");
    }

    fn bitmapset(&mut self, f: &str, v: &BTreeSet<i32>) {
        self.field(f);
        self.s.push_str("(b");
        for x in v {
            let _ = write!(self.s, " {x}");
        }
        self.s.push(')');
    }

    fn oid_list(&mut self, f: &str, v: &[Oid]) {
        self.field(f);
        if v.is_empty() {
            self.s.push_str("<>");
            return;
        }
        self.s.push_str("(o");
        for x in v {
            let _ = write!(self.s, " {x}");
        }
        self.s.push(')');
    }

    fn int_list(&mut self, f: &str, v: &[i32]) {
        self.field(f);
        if v.is_empty() {
            self.s.push_str("<>");
            return;
        }
        self.s.push_str("(i");
        for x in v {
            let _ = write!(self.s, " {x}");
        }
        self.s.push(')');
    }

    fn string_list(&mut self, f: &str, v: &[String]) {
        self.field(f);
        self.strings(v);
    }

    fn strings(&mut self, v: &[String]) {
        if v.is_empty() {
            self.s.push_str("<>");
            return;
        }
        self.s.push('(');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.s.push(' ');
            }
            out_string_node(&mut self.s, x);
        }
        self.s.push(')');
    }

    fn list<T>(&mut self, f: &str, v: &[T], each: impl Fn(&mut Self, &T)) {
        self.field(f);
        self.items(v, each);
    }

    fn items<T>(&mut self, v: &[T], each: impl Fn(&mut Self, &T)) {
        if v.is_empty() {
            self.s.push_str("<>");
            return;
        }
        self.s.push('(');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.s.push(' ');
            }
            each(self, x);
        }
        self.s.push(')');
    }

    fn opt_node(&mut self, f: &str, v: Option<&Node>) {
        self.field(f);
        match v {
            Some(n) => self.node(n),
            None => self.s.push_str("<>"),
        }
    }

    fn node_field(&mut self, f: &str, v: &Node) {
        self.field(f);
        self.node(v);
    }

    fn nodes(&mut self, f: &str, v: &[Node]) {
        self.list(f, v, |o, n| o.node(n));
    }

    fn alias_field(&mut self, f: &str, a: Option<&Alias>) {
        self.field(f);
        match a {
            Some(a) => self.alias(a),
            None => self.s.push_str("<>"),
        }
    }

    pub fn alias(&mut self, a: &Alias) {
        self.s.push('{');
        self.label("ALIAS");
        self.string("aliasname", Some(&a.aliasname));
        self.string_list("colnames", &a.colnames);
        self.s.push('}');
    }

    fn datum(&mut self, d: &Datum) {
        match d {
            Datum::ByVal { bytes, len } => {
                let _ = write!(self.s, "{len} [ ");
                for b in bytes {
                    let _ = write!(self.s, "{} ", *b as i8);
                }
                self.s.push(']');
            }
            Datum::ByRef(bytes) => {
                let _ = write!(self.s, "{} [ ", bytes.len());
                for b in bytes {
                    let _ = write!(self.s, "{} ", *b as i8);
                }
                self.s.push(']');
            }
        }
    }

    pub fn node(&mut self, n: &Node) {
        self.s.push('{');
        match n {
            Node::Var(v) => {
                self.label("VAR");
                self.int("varno", v.varno.into());
                self.int("varattno", v.varattno.into());
                self.uint("vartype", v.vartype);
                self.int("vartypmod", v.vartypmod.into());
                self.uint("varcollid", v.varcollid);
                self.uint("varlevelsup", v.varlevelsup);
                self.uint("varnosyn", v.varnosyn);
                self.int("varattnosyn", v.varattnosyn.into());
                self.loc("location", v.location);
            }
            Node::Const(c) => {
                self.label("CONST");
                self.uint("consttype", c.consttype);
                self.int("consttypmod", c.consttypmod.into());
                self.uint("constcollid", c.constcollid);
                self.int("constlen", c.constlen.into());
                self.boolean("constbyval", c.constbyval);
                self.boolean("constisnull", c.constvalue.is_none());
                self.loc("location", c.location);
                self.s.push_str(" :constvalue ");
                match &c.constvalue {
                    None => self.s.push_str("<>"),
                    Some(d) => self.datum(d),
                }
            }
            Node::Param(p) => {
                self.label("PARAM");
                self.int("paramkind", p.paramkind.into());
                self.int("paramid", p.paramid.into());
                self.uint("paramtype", p.paramtype);
                self.int("paramtypmod", p.paramtypmod.into());
                self.uint("paramcollid", p.paramcollid);
                self.loc("location", p.location);
            }
            Node::Aggref(a) => {
                self.label("AGGREF");
                self.uint("aggfnoid", a.aggfnoid);
                self.uint("aggtype", a.aggtype);
                self.uint("aggcollid", a.aggcollid);
                self.uint("inputcollid", a.inputcollid);
                self.uint("aggtranstype", a.aggtranstype);
                self.oid_list("aggargtypes", &a.aggargtypes);
                self.nodes("aggdirectargs", &a.aggdirectargs);
                self.list("args", &a.args, |o, t| o.target_entry(t));
                self.list("aggorder", &a.aggorder, |o, s| o.sort_group_clause(s));
                self.list("aggdistinct", &a.aggdistinct, |o, s| o.sort_group_clause(s));
                self.opt_node("aggfilter", a.aggfilter.as_ref());
                self.boolean("aggstar", a.aggstar);
                self.boolean("aggvariadic", a.aggvariadic);
                self.chr("aggkind", a.aggkind);
                self.uint("agglevelsup", a.agglevelsup);
                self.int("aggsplit", a.aggsplit.into());
                self.int("aggno", a.aggno.into());
                self.int("aggtransno", a.aggtransno.into());
                self.loc("location", a.location);
            }
            Node::GroupingFunc(g) => {
                self.label("GROUPINGFUNC");
                self.nodes("args", &g.args);
                self.int_list("refs", &g.refs);
                self.int_list("cols", &g.cols);
                self.uint("agglevelsup", g.agglevelsup);
                self.loc("location", g.location);
            }
            Node::WindowFunc(w) => {
                self.label("WINDOWFUNC");
                self.uint("winfnoid", w.winfnoid);
                self.uint("wintype", w.wintype);
                self.uint("wincollid", w.wincollid);
                self.uint("inputcollid", w.inputcollid);
                self.nodes("args", &w.args);
                self.opt_node("aggfilter", w.aggfilter.as_ref());
                self.uint("winref", w.winref);
                self.boolean("winstar", w.winstar);
                self.boolean("winagg", w.winagg);
                self.loc("location", w.location);
            }
            Node::SubscriptingRef(r) => {
                self.label("SUBSCRIPTINGREF");
                self.uint("refcontainertype", r.refcontainertype);
                self.uint("refelemtype", r.refelemtype);
                self.uint("refrestype", r.refrestype);
                self.int("reftypmod", r.reftypmod.into());
                self.uint("refcollid", r.refcollid);
                self.list("refupperindexpr", &r.refupperindexpr, |o, n| match n {
                    Some(n) => o.node(n),
                    None => o.s.push_str("<>"),
                });
                self.field("reflowerindexpr");
                match &r.reflowerindexpr {
                    None => self.s.push_str("<>"),
                    Some(l) => self.items(l, |o, n| match n {
                        Some(n) => o.node(n),
                        None => o.s.push_str("<>"),
                    }),
                }
                self.node_field("refexpr", &r.refexpr);
                self.opt_node("refassgnexpr", r.refassgnexpr.as_ref());
            }
            Node::FuncExpr(f) => {
                self.label("FUNCEXPR");
                self.uint("funcid", f.funcid);
                self.uint("funcresulttype", f.funcresulttype);
                self.boolean("funcretset", f.funcretset);
                self.boolean("funcvariadic", f.funcvariadic);
                self.form("funcformat", f.funcformat);
                self.uint("funccollid", f.funccollid);
                self.uint("inputcollid", f.inputcollid);
                self.nodes("args", &f.args);
                self.loc("location", f.location);
            }
            Node::NamedArgExpr(n) => {
                self.label("NAMEDARGEXPR");
                self.node_field("arg", &n.arg);
                self.string("name", Some(&n.name));
                self.int("argnumber", n.argnumber.into());
                self.loc("location", n.location);
            }
            Node::OpExpr(o) => {
                self.label(match o.kind {
                    OpKind::Op => "OPEXPR",
                    OpKind::Distinct => "DISTINCTEXPR",
                    OpKind::NullIf => "NULLIFEXPR",
                });
                self.uint("opno", o.opno);
                self.uint("opfuncid", o.opfuncid);
                self.uint("opresulttype", o.opresulttype);
                self.boolean("opretset", o.opretset);
                self.uint("opcollid", o.opcollid);
                self.uint("inputcollid", o.inputcollid);
                self.nodes("args", &o.args);
                self.loc("location", o.location);
            }
            Node::ScalarArrayOpExpr(o) => {
                self.label("SCALARARRAYOPEXPR");
                self.uint("opno", o.opno);
                self.uint("opfuncid", o.opfuncid);
                self.uint("hashfuncid", o.hashfuncid);
                self.uint("negfuncid", o.negfuncid);
                self.boolean("useOr", o.use_or);
                self.uint("inputcollid", o.inputcollid);
                self.nodes("args", &o.args);
                self.loc("location", o.location);
            }
            Node::BoolExpr(b) => {
                self.label("BOOLEXPR");
                self.s.push_str(" :boolop ");
                out_token(
                    &mut self.s,
                    Some(match b.boolop {
                        BoolOp::And => "and",
                        BoolOp::Or => "or",
                        BoolOp::Not => "not",
                    }),
                );
                self.nodes("args", &b.args);
                self.loc("location", b.location);
            }
            Node::SubLink(l) => {
                self.label("SUBLINK");
                self.int("subLinkType", l.sub_link_type as i64);
                self.int("subLinkId", l.sub_link_id.into());
                self.opt_node("testexpr", l.testexpr.as_ref());
                self.string_list("operName", &l.oper_name);
                self.field("subselect");
                self.query(&l.subselect);
                self.loc("location", l.location);
            }
            Node::FieldSelect(f) => {
                self.label("FIELDSELECT");
                self.node_field("arg", &f.arg);
                self.int("fieldnum", f.fieldnum.into());
                self.uint("resulttype", f.resulttype);
                self.int("resulttypmod", f.resulttypmod.into());
                self.uint("resultcollid", f.resultcollid);
            }
            Node::RelabelType(r) => {
                self.label("RELABELTYPE");
                self.node_field("arg", &r.arg);
                self.uint("resulttype", r.resulttype);
                self.int("resulttypmod", r.resulttypmod.into());
                self.uint("resultcollid", r.resultcollid);
                self.form("relabelformat", r.relabelformat);
                self.loc("location", r.location);
            }
            Node::CoerceViaIO(c) => {
                self.label("COERCEVIAIO");
                self.node_field("arg", &c.arg);
                self.uint("resulttype", c.resulttype);
                self.uint("resultcollid", c.resultcollid);
                self.form("coerceformat", c.coerceformat);
                self.loc("location", c.location);
            }
            Node::ArrayCoerceExpr(c) => {
                self.label("ARRAYCOERCEEXPR");
                self.node_field("arg", &c.arg);
                self.node_field("elemexpr", &c.elemexpr);
                self.uint("resulttype", c.resulttype);
                self.int("resulttypmod", c.resulttypmod.into());
                self.uint("resultcollid", c.resultcollid);
                self.form("coerceformat", c.coerceformat);
                self.loc("location", c.location);
            }
            Node::ConvertRowtypeExpr(c) => {
                self.label("CONVERTROWTYPEEXPR");
                self.node_field("arg", &c.arg);
                self.uint("resulttype", c.resulttype);
                self.form("convertformat", c.convertformat);
                self.loc("location", c.location);
            }
            Node::CollateExpr(c) => {
                self.label("COLLATEEXPR");
                self.node_field("arg", &c.arg);
                self.uint("collOid", c.coll_oid);
                self.loc("location", c.location);
            }
            Node::CaseExpr(c) => {
                self.label("CASEEXPR");
                self.uint("casetype", c.casetype);
                self.uint("casecollid", c.casecollid);
                self.opt_node("arg", c.arg.as_ref());
                self.list("args", &c.args, |o, w| {
                    o.s.push('{');
                    o.label("CASEWHEN");
                    o.node_field("expr", &w.expr);
                    o.node_field("result", &w.result);
                    o.loc("location", w.location);
                    o.s.push('}');
                });
                self.node_field("defresult", &c.defresult);
                self.loc("location", c.location);
            }
            Node::CaseTestExpr(c) => {
                self.label("CASETESTEXPR");
                self.uint("typeId", c.type_id);
                self.int("typeMod", c.type_mod.into());
                self.uint("collation", c.collation);
            }
            Node::ArrayExpr(a) => {
                self.label("ARRAYEXPR");
                self.uint("array_typeid", a.array_typeid);
                self.uint("array_collid", a.array_collid);
                self.uint("element_typeid", a.element_typeid);
                self.nodes("elements", &a.elements);
                self.boolean("multidims", a.multidims);
                self.loc("location", a.location);
            }
            Node::RowExpr(r) => {
                self.label("ROWEXPR");
                self.nodes("args", &r.args);
                self.uint("row_typeid", r.row_typeid);
                self.form("row_format", r.row_format);
                self.string_list("colnames", &r.colnames);
                self.loc("location", r.location);
            }
            Node::RowCompareExpr(r) => {
                self.label("ROWCOMPAREEXPR");
                self.int("rctype", r.rctype.into());
                self.oid_list("opnos", &r.opnos);
                self.oid_list("opfamilies", &r.opfamilies);
                self.oid_list("inputcollids", &r.inputcollids);
                self.nodes("largs", &r.largs);
                self.nodes("rargs", &r.rargs);
            }
            Node::CoalesceExpr(c) => {
                self.label("COALESCEEXPR");
                self.uint("coalescetype", c.coalescetype);
                self.uint("coalescecollid", c.coalescecollid);
                self.nodes("args", &c.args);
                self.loc("location", c.location);
            }
            Node::MinMaxExpr(m) => {
                self.label("MINMAXEXPR");
                self.uint("minmaxtype", m.minmaxtype);
                self.uint("minmaxcollid", m.minmaxcollid);
                self.uint("inputcollid", m.inputcollid);
                self.int("op", m.op.into());
                self.nodes("args", &m.args);
                self.loc("location", m.location);
            }
            Node::SqlValueFunction(f) => {
                self.label("SQLVALUEFUNCTION");
                self.int("op", f.op as i64);
                self.uint("type", f.type_);
                self.int("typmod", f.typmod.into());
                self.loc("location", f.location);
            }
            Node::NullTest(n) => {
                self.label("NULLTEST");
                self.node_field("arg", &n.arg);
                self.int("nulltesttype", n.nulltesttype.into());
                self.boolean("argisrow", n.argisrow);
                self.loc("location", n.location);
            }
            Node::BooleanTest(b) => {
                self.label("BOOLEANTEST");
                self.node_field("arg", &b.arg);
                self.int("booltesttype", b.booltesttype.into());
                self.loc("location", b.location);
            }
            Node::CoerceToDomain(c) => {
                self.label("COERCETODOMAIN");
                self.node_field("arg", &c.arg);
                self.uint("resulttype", c.resulttype);
                self.int("resulttypmod", c.resulttypmod.into());
                self.uint("resultcollid", c.resultcollid);
                self.form("coercionformat", c.coercionformat);
                self.loc("location", c.location);
            }
            Node::SetToDefault(d) => {
                self.label("SETTODEFAULT");
                self.uint("typeId", d.type_id);
                self.int("typeMod", d.type_mod.into());
                self.uint("collation", d.collation);
                self.loc("location", d.location);
            }
            Node::TargetEntry(t) => {
                // The braces are written by target_entry itself.
                self.s.pop();
                self.target_entry(t);
                return;
            }
        }
        self.s.push('}');
    }

    pub fn target_entry(&mut self, t: &TargetEntry) {
        self.s.push('{');
        self.label("TARGETENTRY");
        self.node_field("expr", &t.expr);
        self.int("resno", t.resno.into());
        self.string("resname", t.resname.as_deref());
        self.uint("ressortgroupref", t.ressortgroupref);
        self.uint("resorigtbl", t.resorigtbl);
        self.int("resorigcol", t.resorigcol.into());
        self.boolean("resjunk", t.resjunk);
        self.s.push('}');
    }

    pub fn sort_group_clause(&mut self, c: &SortGroupClause) {
        self.s.push('{');
        self.label("SORTGROUPCLAUSE");
        self.uint("tleSortGroupRef", c.tle_sort_group_ref);
        self.uint("eqop", c.eqop);
        self.uint("sortop", c.sortop);
        self.boolean("nulls_first", c.nulls_first);
        self.boolean("hashable", c.hashable);
        self.s.push('}');
    }

    fn grouping_set(&mut self, g: &GroupingSet) {
        self.s.push('{');
        self.label("GROUPINGSET");
        self.int("kind", g.kind.into());
        self.field("content");
        let refs: Vec<i32> = g
            .content
            .iter()
            .filter_map(|c| match c {
                GroupingSetContent::Ref(r) => Some(*r),
                GroupingSetContent::Set(_) => None,
            })
            .collect();
        if g.content.is_empty() {
            self.s.push_str("<>");
        } else if refs.len() == g.content.len() {
            self.s.push_str("(i");
            for r in refs {
                let _ = write!(self.s, " {r}");
            }
            self.s.push(')');
        } else {
            self.items(&g.content, |o, c| match c {
                GroupingSetContent::Set(s) => o.grouping_set(s),
                GroupingSetContent::Ref(r) => {
                    let _ = write!(o.s, "{r}");
                }
            });
        }
        self.loc("location", g.location);
        self.s.push('}');
    }

    fn window_clause(&mut self, w: &WindowClause) {
        self.s.push('{');
        self.label("WINDOWCLAUSE");
        self.string("name", w.name.as_deref());
        self.string("refname", w.refname.as_deref());
        self.list("partitionClause", &w.partition_clause, |o, s| {
            o.sort_group_clause(s)
        });
        self.list("orderClause", &w.order_clause, |o, s| {
            o.sort_group_clause(s)
        });
        self.int("frameOptions", w.frame_options.into());
        self.opt_node("startOffset", w.start_offset.as_ref());
        self.opt_node("endOffset", w.end_offset.as_ref());
        self.field("runCondition");
        self.s.push_str("<>");
        self.uint("startInRangeFunc", w.start_in_range_func);
        self.uint("endInRangeFunc", w.end_in_range_func);
        self.uint("inRangeColl", w.in_range_coll);
        self.boolean("inRangeAsc", w.in_range_asc);
        self.boolean("inRangeNullsFirst", w.in_range_nulls_first);
        self.uint("winref", w.winref);
        self.boolean("copiedOrder", w.copied_order);
        self.s.push('}');
    }

    fn jointree_node(&mut self, n: &JoinTreeNode) {
        match n {
            JoinTreeNode::RangeTblRef(i) => {
                let _ = write!(self.s, "{{RANGETBLREF :rtindex {i}}}");
            }
            JoinTreeNode::JoinExpr(j) => {
                self.s.push('{');
                self.label("JOINEXPR");
                self.int("jointype", j.jointype.into());
                self.boolean("isNatural", j.is_natural);
                self.field("larg");
                self.jointree_node(&j.larg);
                self.field("rarg");
                self.jointree_node(&j.rarg);
                self.string_list("usingClause", &j.using_clause);
                self.alias_field("join_using_alias", j.join_using_alias.as_ref());
                self.opt_node("quals", j.quals.as_ref());
                self.alias_field("alias", j.alias.as_ref());
                self.int("rtindex", j.rtindex.into());
                self.s.push('}');
            }
        }
    }

    fn fromexpr(&mut self, f: &FromExpr) {
        self.s.push('{');
        self.label("FROMEXPR");
        self.list("fromlist", &f.fromlist, |o, n| o.jointree_node(n));
        self.opt_node("quals", f.quals.as_ref());
        self.s.push('}');
    }

    fn setop_node(&mut self, n: &SetOpNode) {
        match n {
            SetOpNode::RangeTblRef(i) => {
                let _ = write!(self.s, "{{RANGETBLREF :rtindex {i}}}");
            }
            SetOpNode::SetOp(s) => self.set_operation(s),
        }
    }

    fn set_operation(&mut self, s: &SetOperationStmt) {
        self.s.push('{');
        self.label("SETOPERATIONSTMT");
        self.int("op", s.op.into());
        self.boolean("all", s.all);
        self.field("larg");
        self.setop_node(&s.larg);
        self.field("rarg");
        self.setop_node(&s.rarg);
        self.oid_list("colTypes", &s.col_types);
        self.int_list("colTypmods", &s.col_typmods);
        self.oid_list("colCollations", &s.col_collations);
        self.list("groupClauses", &s.group_clauses, |o, c| {
            o.sort_group_clause(c)
        });
        self.s.push('}');
    }

    fn cte(&mut self, c: &CommonTableExpr) {
        self.s.push('{');
        self.label("COMMONTABLEEXPR");
        self.string("ctename", Some(&c.ctename));
        self.string_list("aliascolnames", &c.aliascolnames);
        self.int("ctematerialized", c.ctematerialized.into());
        self.field("ctequery");
        self.query(&c.ctequery);
        self.field("search_clause");
        self.s.push_str("<>");
        self.field("cycle_clause");
        self.s.push_str("<>");
        self.loc("location", c.location);
        self.boolean("cterecursive", c.cterecursive);
        self.int("cterefcount", c.cterefcount.into());
        self.string_list("ctecolnames", &c.ctecolnames);
        self.oid_list("ctecoltypes", &c.ctecoltypes);
        self.int_list("ctecoltypmods", &c.ctecoltypmods);
        self.oid_list("ctecolcollations", &c.ctecolcollations);
        self.s.push('}');
    }

    fn rte(&mut self, r: &RangeTblEntry) {
        self.s.push('{');
        self.label("RANGETBLENTRY");
        self.alias_field("alias", r.alias.as_ref());
        self.field("eref");
        self.alias(&r.eref);
        let kind = match &r.kind {
            RteKind::Relation { .. } => 0,
            RteKind::Subquery { .. } => 1,
            RteKind::Join { .. } => 2,
            RteKind::Function { .. } => 3,
            RteKind::Values { .. } => 5,
            RteKind::Cte { .. } => 6,
            RteKind::Result => 8,
        };
        self.int("rtekind", kind);
        match &r.kind {
            RteKind::Relation {
                relid,
                relkind,
                rellockmode,
            } => {
                self.uint("relid", *relid);
                self.chr("relkind", *relkind);
                self.int("rellockmode", (*rellockmode).into());
                self.field("tablesample");
                self.s.push_str("<>");
            }
            RteKind::Subquery {
                subquery,
                security_barrier,
            } => {
                self.field("subquery");
                self.query(subquery);
                self.boolean("security_barrier", *security_barrier);
            }
            RteKind::Join {
                jointype,
                joinmergedcols,
                joinaliasvars,
                joinleftcols,
                joinrightcols,
                join_using_alias,
            } => {
                self.int("jointype", (*jointype).into());
                self.int("joinmergedcols", (*joinmergedcols).into());
                self.nodes("joinaliasvars", joinaliasvars);
                self.int_list("joinleftcols", joinleftcols);
                self.int_list("joinrightcols", joinrightcols);
                self.alias_field("join_using_alias", join_using_alias.as_ref());
            }
            RteKind::Function {
                functions,
                funcordinality,
            } => {
                self.list("functions", functions, |o, f| {
                    o.s.push('{');
                    o.label("RANGETBLFUNCTION");
                    o.node_field("funcexpr", &f.funcexpr);
                    o.int("funccolcount", f.funccolcount.into());
                    o.string_list("funccolnames", &f.funccolnames);
                    o.oid_list("funccoltypes", &f.funccoltypes);
                    o.int_list("funccoltypmods", &f.funccoltypmods);
                    o.oid_list("funccolcollations", &f.funccolcollations);
                    o.field("funcparams");
                    o.s.push_str("(b)");
                    o.s.push('}');
                });
                self.boolean("funcordinality", *funcordinality);
            }
            RteKind::Values {
                values_lists,
                coltypes,
                coltypmods,
                colcollations,
            } => {
                self.list("values_lists", values_lists, |o, l| {
                    o.items(l, |o, n| o.node(n))
                });
                self.oid_list("coltypes", coltypes);
                self.int_list("coltypmods", coltypmods);
                self.oid_list("colcollations", colcollations);
            }
            RteKind::Cte {
                ctename,
                ctelevelsup,
                self_reference,
                coltypes,
                coltypmods,
                colcollations,
            } => {
                self.string("ctename", Some(ctename));
                self.uint("ctelevelsup", *ctelevelsup);
                self.boolean("self_reference", *self_reference);
                self.oid_list("coltypes", coltypes);
                self.int_list("coltypmods", coltypmods);
                self.oid_list("colcollations", colcollations);
            }
            RteKind::Result => {}
        }
        self.boolean("lateral", r.lateral);
        self.boolean("inh", r.inh);
        self.boolean("inFromCl", r.in_from_cl);
        self.uint("requiredPerms", r.required_perms);
        self.uint("checkAsUser", r.check_as_user);
        self.bitmapset("selectedCols", &r.selected_cols);
        self.bitmapset("insertedCols", &r.inserted_cols);
        self.bitmapset("updatedCols", &r.updated_cols);
        self.bitmapset("extraUpdatedCols", &BTreeSet::new());
        self.field("securityQuals");
        self.s.push_str("<>");
        self.s.push('}');
    }

    pub fn query(&mut self, q: &Query) {
        self.s.push('{');
        self.label("QUERY");
        self.int("commandType", q.command_type.into());
        self.int("querySource", q.query_source.into());
        self.boolean("canSetTag", q.can_set_tag);
        self.field("utilityStmt");
        match &q.utility_stmt {
            None => self.s.push_str("<>"),
            Some(UtilityStmt::Notify {
                conditionname,
                payload,
            }) => {
                self.s.push('{');
                self.label("NOTIFYSTMT");
                self.string("conditionname", Some(conditionname));
                self.string("payload", payload.as_deref());
                self.s.push('}');
            }
        }
        self.int("resultRelation", q.result_relation.into());
        self.boolean("hasAggs", q.has_aggs);
        self.boolean("hasWindowFuncs", q.has_window_funcs);
        self.boolean("hasTargetSRFs", q.has_target_srfs);
        self.boolean("hasSubLinks", q.has_sub_links);
        self.boolean("hasDistinctOn", q.has_distinct_on);
        self.boolean("hasRecursive", q.has_recursive);
        self.boolean("hasModifyingCTE", q.has_modifying_cte);
        self.boolean("hasForUpdate", q.has_for_update);
        self.boolean("hasRowSecurity", q.has_row_security);
        self.boolean("isReturn", q.is_return);
        self.list("cteList", &q.cte_list, |o, c| o.cte(c));
        self.list("rtable", &q.rtable, |o, r| o.rte(r));
        self.field("jointree");
        match &q.jointree {
            None => self.s.push_str("<>"),
            Some(j) => self.fromexpr(j),
        }
        self.list("targetList", &q.target_list, |o, t| o.target_entry(t));
        self.int("override", q.override_.into());
        self.field("onConflict");
        match &q.on_conflict {
            None => self.s.push_str("<>"),
            Some(oc) => {
                self.s.push('{');
                self.label("ONCONFLICTEXPR");
                self.int("action", oc.action.into());
                self.list("arbiterElems", &oc.arbiter_elems, |o, e| {
                    o.s.push('{');
                    o.label("INFERENCEELEM");
                    o.field("expr");
                    o.node(&e.expr);
                    o.uint("infercollid", e.infercollid);
                    o.uint("inferopclass", e.inferopclass);
                    o.s.push('}');
                });
                self.opt_node("arbiterWhere", oc.arbiter_where.as_ref());
                self.uint("constraint", oc.constraint);
                self.field("onConflictSet");
                self.s.push_str("<>");
                self.field("onConflictWhere");
                self.s.push_str("<>");
                self.int("exclRelIndex", 0);
                self.field("exclRelTlist");
                self.s.push_str("<>");
                self.s.push('}');
            }
        }
        self.list("returningList", &q.returning_list, |o, t| o.target_entry(t));
        self.list("groupClause", &q.group_clause, |o, c| {
            o.sort_group_clause(c)
        });
        self.boolean("groupDistinct", q.group_distinct);
        self.list("groupingSets", &q.grouping_sets, |o, g| o.grouping_set(g));
        self.opt_node("havingQual", q.having_qual.as_ref());
        self.list("windowClause", &q.window_clause, |o, w| o.window_clause(w));
        self.list("distinctClause", &q.distinct_clause, |o, c| {
            o.sort_group_clause(c)
        });
        self.list("sortClause", &q.sort_clause, |o, c| o.sort_group_clause(c));
        self.opt_node("limitOffset", q.limit_offset.as_ref());
        self.opt_node("limitCount", q.limit_count.as_ref());
        self.int("limitOption", q.limit_option.into());
        self.list("rowMarks", &q.row_marks, |o, r| {
            o.s.push('{');
            o.label("ROWMARKCLAUSE");
            o.uint("rti", r.rti);
            o.int("strength", r.strength.into());
            o.int("waitPolicy", r.wait_policy.into());
            o.boolean("pushedDown", r.pushed_down);
            o.s.push('}');
        });
        self.field("setOperations");
        match &q.set_operations {
            None => self.s.push_str("<>"),
            Some(s) => self.set_operation(s),
        }
        self.oid_list("constraintDeps", &q.constraint_deps);
        self.field("withCheckOptions");
        self.s.push_str("<>");
        self.field("mergeActionList");
        self.s.push_str("<>");
        self.boolean("mergeUseOuterJoin", false);
        self.loc("stmt_location", q.stmt_location);
        self.int("stmt_len", q.stmt_len.into());
        self.s.push('}');
    }
}

/// `outToken`.
pub(crate) fn out_token(s: &mut String, v: Option<&str>) {
    let Some(v) = v.filter(|v| !v.is_empty()) else {
        s.push_str("<>");
        return;
    };
    let bytes = v.as_bytes();
    let first = bytes[0];
    if first == b'<'
        || first == b'"'
        || first.is_ascii_digit()
        || ((first == b'+' || first == b'-')
            && bytes
                .get(1)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'.'))
    {
        s.push('\\');
    }
    for c in v.chars() {
        if matches!(c, ' ' | '\n' | '\t' | '(' | ')' | '{' | '}' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
}

/// `_outString`: a String node.
fn out_string_node(s: &mut String, v: &str) {
    s.push('"');
    if !v.is_empty() {
        out_token(s, Some(v));
    }
    s.push('"');
}

/// `nodeToString` of a list of queries (a rule's `ev_action`).
pub fn queries_to_string(qs: &[Query]) -> String {
    let mut o = Out::new();
    o.items(qs, |o, q| o.query(q));
    o.s
}

/// `nodeToString` of one expression (a rule's `ev_qual`), `<>` for none.
pub fn node_to_string(n: Option<&Node>) -> String {
    let mut o = Out::new();
    match n {
        Some(n) => o.node(n),
        None => o.s.push_str("<>"),
    }
    o.s
}

/// `equal()` on two expressions.
pub(crate) fn equal(a: &Node, b: &Node) -> bool {
    let mut x = Out::for_equal();
    x.node(a);
    let mut y = Out::for_equal();
    y.node(b);
    x.s == y.s
}
