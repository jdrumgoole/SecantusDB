//! `parse_expr.c`: `transformExpr` and its helpers, and `parse_node.c`'s
//! `make_const` and container subscripting.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_location, expr_type, expr_typmod};
use super::nodes::*;
use super::parse::{Analyzer, ExprKind};
use super::{unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

impl<'a> Analyzer<'a> {
    /// `transformExpr`.
    pub fn transform_expr(&mut self, n: &pb::Node, kind: ExprKind) -> R<Node> {
        let sv = self.ps().expr_kind;
        self.ps().expr_kind = kind;
        let r = self.transform_expr_recurse(n);
        self.ps().expr_kind = sv;
        r
    }

    /// `transformExprRecurse`.
    pub fn transform_expr_recurse(&mut self, n: &pb::Node) -> R<Node> {
        let Some(node) = n.node.as_ref() else {
            return unsupported("an empty expression");
        };
        match node {
            N::ColumnRef(c) => self.transform_column_ref(c),
            N::ParamRef(_) => unsupported("there is no parameter $n"),
            N::AConst(c) => self.make_const(c),
            N::AIndirection(i) => self.transform_indirection(i),
            N::AArrayExpr(a) => self.transform_array_expr(a, 0, 0, -1),
            N::TypeCast(tc) => self.transform_type_cast(tc),
            N::CollateClause(c) => self.transform_collate_clause(c),
            N::AExpr(a) => self.transform_aexpr(a),
            N::BoolExpr(b) => self.transform_bool_expr(b),
            N::FuncCall(f) => self.transform_func_call(f),
            N::NamedArgExpr(na) => {
                let arg = na
                    .arg
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a named argument".into()))?;
                let arg = self.transform_expr_recurse(arg)?;
                Ok(Node::NamedArgExpr(Box::new(NamedArgExpr {
                    arg,
                    name: na.name.clone(),
                    argnumber: na.argnumber,
                    location: na.location,
                })))
            }
            N::SubLink(s) => self.transform_sub_link(s),
            N::GroupingFunc(g) => self.transform_grouping_func(g),
            N::CaseExpr(c) => self.transform_case_expr(c),
            N::RowExpr(r) => self.transform_row_expr(r, false),
            N::CoalesceExpr(c) => self.transform_coalesce_expr(c),
            N::MinMaxExpr(m) => self.transform_min_max_expr(m),
            N::SqlvalueFunction(f) => {
                Ok(self.transform_sql_value_function(f.op - 1, f.typmod, f.location))
            }
            N::NullTest(t) => {
                let arg = t
                    .arg
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a null test".into()))?;
                let arg = self.transform_expr_recurse(arg)?;
                let argisrow = self.cat.type_is_rowtype(expr_type(self.cat, &arg));
                Ok(Node::NullTest(Box::new(NullTest {
                    arg,
                    nulltesttype: t.nulltesttype - 1,
                    argisrow,
                    location: t.location,
                })))
            }
            N::BooleanTest(b) => {
                let arg = b
                    .arg
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a boolean test".into()))?;
                let arg = self.transform_expr_recurse(arg)?;
                let arg = self.coerce_to_boolean(arg, "IS TRUE")?;
                Ok(Node::BooleanTest(Box::new(BooleanTest {
                    arg,
                    booltesttype: b.booltesttype - 1,
                    location: b.location,
                })))
            }
            N::SetToDefault(_) => unsupported("DEFAULT is not allowed in this context"),
            N::MultiAssignRef(m) => self.transform_multi_assign_ref(m),
            _ => unsupported("this expression in a stored rule"),
        }
    }

    /// `transformMultiAssignRef` for a ROW source: each column of
    /// `SET (a, b) = (x, y)` is its element of the row (a sub-SELECT source,
    /// a MULTIEXPR SubLink, is not transcribed).
    fn transform_multi_assign_ref(&mut self, m: &pb::MultiAssignRef) -> R<Node> {
        let Some(N::RowExpr(r)) = m.source.as_deref().and_then(|s| s.node.as_ref()) else {
            return unsupported("a multiple-column UPDATE from a sub-SELECT in a stored rule");
        };
        if r.args.len() != m.ncolumns as usize {
            return Err(AnalysisError(
                "number of columns does not match number of values".into(),
            ));
        }
        // The first column parks the row in p_multiassign_exprs, which takes
        // a result number.
        if m.colno == 1 {
            self.ps().next_resno += 1;
        }
        let e = r
            .args
            .get((m.colno - 1) as usize)
            .ok_or_else(|| AnalysisError("a multiple-assignment column".into()))?;
        if let Some(N::SetToDefault(d)) = e.node.as_ref() {
            return Ok(Node::SetToDefault(SetToDefault {
                type_id: 0,
                type_mod: -1,
                collation: 0,
                location: d.location,
            }));
        }
        self.transform_expr_recurse(e)
    }

    /// `transformColumnRef`.
    fn transform_column_ref(&mut self, cref: &pb::ColumnRef) -> R<Node> {
        let fields: Vec<Option<&str>> = cref
            .fields
            .iter()
            .map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect();
        let star = |i: usize| matches!(cref.fields[i].node.as_ref(), Some(N::AStar(_)));
        let loc = cref.location;
        let node = match fields.len() {
            1 => {
                let colname = fields[0].ok_or_else(|| AnalysisError("a column name".into()))?;
                match self.col_name_to_var(colname, false, loc)? {
                    Some(v) => Some(v),
                    None => match self.refname_namespace_item(None, colname)? {
                        Some((up, idx)) => Some(self.transform_whole_row_ref(up, idx, loc)?),
                        None => None,
                    },
                }
            }
            2..=4 => {
                let (nsp, rel, last) = match fields.len() {
                    2 => (None, fields[0], 1),
                    3 => (fields[0], fields[1], 2),
                    _ => {
                        if fields[0] != Some(self.cat.data.database.as_str()) {
                            return unsupported("cross-database references are not implemented");
                        }
                        (fields[1], fields[2], 3)
                    }
                };
                let rel = rel.ok_or_else(|| AnalysisError("a relation name".into()))?;
                let Some((up, idx)) = self.refname_namespace_item(nsp, rel)? else {
                    return unsupported(format!("missing FROM-clause entry for table \"{rel}\""));
                };
                if star(last) {
                    Some(self.transform_whole_row_ref(up, idx, loc)?)
                } else {
                    let colname = fields[last].ok_or_else(|| AnalysisError("a column".into()))?;
                    match self.scan_nsitem_for_column(up, idx, colname, loc)? {
                        Some(v) => Some(v),
                        None => {
                            let whole = self.transform_whole_row_ref(up, idx, loc)?;
                            self.parse_func_or_column(
                                &[colname.to_string()],
                                vec![whole],
                                None,
                                loc,
                            )?
                        }
                    }
                }
            }
            _ => return unsupported("improper qualified name (too many dotted names)"),
        };
        node.ok_or_else(|| {
            AnalysisError(format!(
                "column \"{}\" does not exist",
                fields
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(".")
            ))
        })
    }

    /// `transformWholeRowRef`.
    fn transform_whole_row_ref(&mut self, up: usize, idx: usize, location: i32) -> R<Node> {
        let ns = self.level_ref(up).namespace[idx].clone();
        let rte = self.level_ref(up).rtable[(ns.rtindex - 1) as usize].clone();
        if ns.is_eref {
            let v = self.make_whole_row_var(&rte, ns.rtindex, up, location)?;
            if let Node::Var(var) = &v {
                self.mark_var_for_select_priv(var);
            }
            return Ok(v);
        }
        let (_, mut fields) = self.expand_rte(&rte, ns.rtindex, up, location)?;
        fields.truncate(ns.names.colnames.len());
        Ok(Node::RowExpr(Box::new(RowExpr {
            args: fields,
            row_typeid: RECORDOID,
            row_format: CoercionForm::ImplicitCast,
            colnames: ns.names.colnames.clone(),
            location,
        })))
    }

    /// `makeWholeRowVar` (allowScalar = true).
    fn make_whole_row_var(
        &self,
        rte: &RangeTblEntry,
        varno: i32,
        up: usize,
        location: i32,
    ) -> R<Node> {
        let mk = |t: Oid, m: i32, c: Oid, attno: i32| Var {
            varno,
            varattno: attno,
            vartype: t,
            vartypmod: m,
            varcollid: c,
            varlevelsup: up as u32,
            varnosyn: varno as u32,
            varattnosyn: attno,
            location,
        };
        let v = match &rte.kind {
            RteKind::Relation { relid, .. } => {
                let reltype = self.cat.rel(*relid).map_or(RECORDOID, |r| {
                    if r.reltype != 0 {
                        r.reltype
                    } else {
                        RECORDOID
                    }
                });
                mk(reltype, -1, 0, 0)
            }
            RteKind::Function {
                functions,
                funcordinality,
            } => {
                if functions.len() == 1 && !funcordinality {
                    let f = &functions[0];
                    let t = expr_type(self.cat, &f.funcexpr);
                    if self.cat.type_or_domain_type_relid(t) != 0 {
                        mk(t, -1, 0, 0)
                    } else if !f.funccolnames.is_empty() || t == RECORDOID {
                        mk(RECORDOID, -1, 0, 0)
                    } else {
                        // A scalar function: a plain reference to its value.
                        mk(
                            t,
                            expr_typmod(self.cat, &f.funcexpr),
                            expr_collation(self.cat, &f.funcexpr),
                            1,
                        )
                    }
                } else {
                    mk(RECORDOID, -1, 0, 0)
                }
            }
            _ => mk(RECORDOID, -1, 0, 0),
        };
        Ok(Node::Var(v))
    }

    /// `make_const`.
    pub fn make_const(&mut self, c: &pb::AConst) -> R<Node> {
        if c.isnull {
            return Ok(Node::Const(Const {
                consttype: UNKNOWNOID,
                consttypmod: -1,
                constcollid: 0,
                constlen: -2,
                constbyval: false,
                constvalue: None,
                location: c.location,
            }));
        }
        let (t, len, byval, val) = match c.val.as_ref() {
            Some(pb::a_const::Val::Ival(i)) => {
                (INT4OID, 4, true, Datum::from_i64(i.ival.into(), 4))
            }
            Some(pb::a_const::Val::Fval(f)) => match f.fval.parse::<i64>() {
                Ok(v) if v == i64::from(v as i32) => (INT4OID, 4, true, Datum::from_i64(v, 4)),
                Ok(v) => (INT8OID, 8, true, Datum::from_i64(v, 8)),
                Err(_) => (
                    NUMERICOID,
                    -1,
                    false,
                    super::datum::numeric_in(&f.fval, -1)?,
                ),
            },
            Some(pb::a_const::Val::Boolval(b)) => {
                (BOOLOID, 1, true, Datum::from_i64(i64::from(b.boolval), 1))
            }
            Some(pb::a_const::Val::Sval(s)) => {
                let mut bytes = s.sval.as_bytes().to_vec();
                bytes.push(0);
                (UNKNOWNOID, -2, false, Datum::ByRef(bytes))
            }
            Some(pb::a_const::Val::Bsval(b)) => {
                (1560, -1, false, super::datum::bit_in(&b.bsval, -1)?)
            }
            None => return unsupported("a constant without a value"),
        };
        Ok(Node::Const(Const {
            consttype: t,
            consttypmod: -1,
            constcollid: 0,
            constlen: len,
            constbyval: byval,
            constvalue: Some(val),
            location: c.location,
        }))
    }

    /// `transformIndirection`.
    fn transform_indirection(&mut self, ind: &pb::AIndirection) -> R<Node> {
        let arg = ind
            .arg
            .as_deref()
            .ok_or_else(|| AnalysisError("an indirection".into()))?;
        let mut result = self.transform_expr_recurse(arg)?;
        let location = expr_location(&result);
        let mut subscripts: Vec<pb::AIndices> = Vec::new();
        for n in &ind.indirection {
            match n.node.as_ref() {
                Some(N::AIndices(i)) => subscripts.push((**i).clone()),
                Some(N::AStar(_)) => {
                    return unsupported("row expansion via \"*\" is not supported here")
                }
                Some(N::String(s)) => {
                    if !subscripts.is_empty() {
                        let t = expr_type(self.cat, &result);
                        let m = expr_typmod(self.cat, &result);
                        result = self.transform_container_subscripts(
                            result,
                            t,
                            m,
                            &std::mem::take(&mut subscripts),
                        )?;
                    }
                    let name = s.sval.clone();
                    result = self
                        .parse_func_or_column(
                            std::slice::from_ref(&name),
                            vec![result],
                            None,
                            location,
                        )?
                        .ok_or_else(|| AnalysisError(format!("column \"{name}\" not found")))?;
                }
                _ => return unsupported("this indirection"),
            }
        }
        if !subscripts.is_empty() {
            let t = expr_type(self.cat, &result);
            let m = expr_typmod(self.cat, &result);
            result = self.transform_container_subscripts(result, t, m, &subscripts)?;
        }
        Ok(result)
    }

    /// `transformContainerSubscripts` with the array subscripting
    /// routines (`arraysubs.c`'s `array_subscript_transform`).
    fn transform_container_subscripts(
        &mut self,
        base: Node,
        container_type: Oid,
        container_typmod: i32,
        indirection: &[pb::AIndices],
    ) -> R<Node> {
        let mut typmod = container_typmod;
        let mut ctype = self.cat.base_type_and_typmod(container_type, &mut typmod);
        if ctype == INT2VECTOROID {
            ctype = INT2ARRAYOID;
        } else if ctype == OIDVECTOROID {
            ctype = OIDARRAYOID;
        }
        let row = self
            .cat
            .type_row(ctype)
            .ok_or_else(|| AnalysisError("a container type".into()))?;
        if row.subscript != F_ARRAY_SUBSCRIPT_HANDLER {
            return unsupported("subscripting a non-array type");
        }
        let elemtype = row.elem;
        let is_slice = indirection.iter().any(|a| a.is_slice);
        let mut upper = Vec::new();
        let mut lower = Vec::new();
        for ai in indirection {
            if is_slice {
                match ai.lidx.as_deref() {
                    Some(l) => {
                        let e = self.transform_expr_recurse(l)?;
                        lower.push(Some(self.subscript_to_int4(e)?));
                    }
                    None if !ai.is_slice => lower.push(Some(Node::Const(Const {
                        consttype: INT4OID,
                        consttypmod: -1,
                        constcollid: 0,
                        constlen: 4,
                        constbyval: true,
                        constvalue: Some(Datum::from_i64(1, 4)),
                        location: -1,
                    }))),
                    None => lower.push(None),
                }
            }
            match ai.uidx.as_deref() {
                Some(u) => {
                    let e = self.transform_expr_recurse(u)?;
                    upper.push(Some(self.subscript_to_int4(e)?));
                }
                None => upper.push(None),
            }
        }
        let restype = if is_slice { ctype } else { elemtype };
        let reftypmod = if is_slice { typmod } else { -1 };
        let _ = reftypmod;
        Ok(Node::SubscriptingRef(Box::new(SubscriptingRef {
            refcontainertype: ctype,
            refelemtype: elemtype,
            refrestype: restype,
            reftypmod: typmod,
            refcollid: 0,
            refupperindexpr: upper,
            reflowerindexpr: is_slice.then_some(lower),
            refexpr: base,
            refassgnexpr: None,
        })))
    }

    fn subscript_to_int4(&mut self, e: Node) -> R<Node> {
        let t = expr_type(self.cat, &e);
        let r = self.coerce_to_target_type(
            e,
            t,
            INT4OID,
            -1,
            CoercionContext::Assignment,
            CoercionForm::ImplicitCast,
            -1,
        )?;
        r.ok_or_else(|| AnalysisError("array subscript must have type integer".into()))
    }

    /// `transformAExpr*`.
    fn transform_aexpr(&mut self, a: &pb::AExpr) -> R<Node> {
        use pb::AExprKind as K;
        let kind = pb::AExprKind::try_from(a.kind).unwrap_or(K::Undefined);
        let name = super::clause::string_list(&a.name);
        match kind {
            K::AexprOp | K::AexprLike | K::AexprIlike | K::AexprSimilar => {
                let lexpr = a.lexpr.as_deref();
                let rexpr = a.rexpr.as_deref();
                if let (Some(l), Some(r)) = (lexpr, rexpr) {
                    if matches!(l.node, Some(N::RowExpr(_))) {
                        if let Some(N::SubLink(s)) = r.node.as_ref() {
                            if s.sub_link_type == pb::SubLinkType::ExprSublink as i32 {
                                let mut s = (**s).clone();
                                s.sub_link_type = pb::SubLinkType::RowcompareSublink as i32;
                                s.testexpr = Some(Box::new(l.clone()));
                                s.oper_name = a.name.clone();
                                s.location = a.location;
                                return self.transform_sub_link(&s);
                            }
                        }
                        if matches!(r.node, Some(N::RowExpr(_))) {
                            let l = self.transform_expr_recurse(l)?;
                            let r = self.transform_expr_recurse(r)?;
                            let (Node::RowExpr(lr), Node::RowExpr(rr)) = (l, r) else {
                                return unsupported("a row comparison");
                            };
                            return self
                                .make_row_comparison_op(&name, lr.args, rr.args, a.location);
                        }
                    }
                }
                let l = match lexpr {
                    Some(l) => Some(self.transform_expr_recurse(l)?),
                    None => None,
                };
                let r = self
                    .transform_expr_recurse(rexpr.ok_or_else(|| {
                        AnalysisError("an operator without its argument".into())
                    })?)?;
                self.make_op(&name, l, r, a.location)
            }
            K::AexprOpAny | K::AexprOpAll => {
                let l = self.transform_expr_recurse(
                    a.lexpr
                        .as_deref()
                        .ok_or_else(|| AnalysisError("ANY without its left side".into()))?,
                )?;
                let r = self.transform_expr_recurse(
                    a.rexpr
                        .as_deref()
                        .ok_or_else(|| AnalysisError("ANY without its array".into()))?,
                )?;
                self.make_scalar_array_op(&name, kind == K::AexprOpAny, l, r, a.location)
            }
            K::AexprDistinct | K::AexprNotDistinct => self.transform_aexpr_distinct(a, &name),
            K::AexprNullif => {
                let l = self.transform_expr_recurse(
                    a.lexpr
                        .as_deref()
                        .ok_or_else(|| AnalysisError("NULLIF".into()))?,
                )?;
                let r = self.transform_expr_recurse(
                    a.rexpr
                        .as_deref()
                        .ok_or_else(|| AnalysisError("NULLIF".into()))?,
                )?;
                let op = self.make_op(&name, Some(l), r, a.location)?;
                let Node::OpExpr(mut o) = op else {
                    return unsupported("NULLIF's operator");
                };
                if o.opresulttype != BOOLOID {
                    return unsupported("NULLIF requires = operator to yield boolean");
                }
                o.opresulttype = expr_type(self.cat, &o.args[0]);
                o.kind = OpKind::NullIf;
                Ok(Node::OpExpr(o))
            }
            K::AexprIn => self.transform_aexpr_in(a, &name),
            K::AexprBetween | K::AexprNotBetween | K::AexprBetweenSym | K::AexprNotBetweenSym => {
                self.transform_aexpr_between(a, kind)
            }
            _ => unsupported("this operator form"),
        }
    }

    /// `transformAExprDistinct`.
    fn transform_aexpr_distinct(&mut self, a: &pb::AExpr, name: &[String]) -> R<Node> {
        let not = a.kind == pb::AExprKind::AexprNotDistinct as i32;
        let is_null_const = |n: Option<&pb::Node>| matches!(n.and_then(|n| n.node.as_ref()), Some(N::AConst(c)) if c.isnull);
        let lexpr = a.lexpr.as_deref();
        let rexpr = a.rexpr.as_deref();
        let nulltest = |me: &mut Self, arg: &pb::Node| -> R<Node> {
            let arg = me.transform_expr_recurse(arg)?;
            Ok(Node::NullTest(Box::new(NullTest {
                arg,
                nulltesttype: if not { 0 } else { 1 },
                argisrow: false,
                location: a.location,
            })))
        };
        if is_null_const(rexpr) {
            return nulltest(self, lexpr.ok_or_else(|| AnalysisError("DISTINCT".into()))?);
        }
        if is_null_const(lexpr) {
            return nulltest(self, rexpr.ok_or_else(|| AnalysisError("DISTINCT".into()))?);
        }
        let l =
            self.transform_expr_recurse(lexpr.ok_or_else(|| AnalysisError("DISTINCT".into()))?)?;
        let r =
            self.transform_expr_recurse(rexpr.ok_or_else(|| AnalysisError("DISTINCT".into()))?)?;
        let result = match (l, r) {
            (Node::RowExpr(lr), Node::RowExpr(rr)) => {
                let mut result: Option<Node> = None;
                if lr.args.len() != rr.args.len() {
                    return unsupported("unequal number of entries in row expressions");
                }
                for (x, y) in lr.args.into_iter().zip(rr.args) {
                    let cmp = self.make_distinct_op(name, x, y, a.location)?;
                    result = Some(match result {
                        None => cmp,
                        Some(prev) => Node::BoolExpr(Box::new(BoolExpr {
                            boolop: BoolOp::Or,
                            args: vec![prev, cmp],
                            location: a.location,
                        })),
                    });
                }
                match result {
                    Some(r) => r,
                    None => bool_const(false),
                }
            }
            (l, r) => self.make_distinct_op(name, l, r, a.location)?,
        };
        Ok(if not {
            Node::BoolExpr(Box::new(BoolExpr {
                boolop: BoolOp::Not,
                args: vec![result],
                location: a.location,
            }))
        } else {
            result
        })
    }

    fn make_distinct_op(&mut self, name: &[String], l: Node, r: Node, location: i32) -> R<Node> {
        let op = self.make_op(name, Some(l), r, location)?;
        let Node::OpExpr(mut o) = op else {
            return unsupported("IS DISTINCT FROM's operator");
        };
        if o.opresulttype != BOOLOID {
            return unsupported("IS DISTINCT FROM requires = operator to yield boolean");
        }
        o.kind = OpKind::Distinct;
        Ok(Node::OpExpr(o))
    }

    /// `transformAExprIn`.
    fn transform_aexpr_in(&mut self, a: &pb::AExpr, name: &[String]) -> R<Node> {
        let use_or = name.first().map(String::as_str) != Some("<>");
        let lexpr = self.transform_expr_recurse(
            a.lexpr
                .as_deref()
                .ok_or_else(|| AnalysisError("IN without its left side".into()))?,
        )?;
        let items: Vec<pb::Node> = match a.rexpr.as_deref().and_then(|r| r.node.as_ref()) {
            Some(N::List(l)) => l.items.clone(),
            _ => return unsupported("an IN list"),
        };
        let mut rexprs = Vec::new();
        let mut rvars = Vec::new();
        let mut rnonvars = Vec::new();
        for item in &items {
            let r = self.transform_expr_recurse(item)?;
            if super::nodefuncs::contain_vars_of_level(&r, 0) {
                rvars.push(r.clone());
            } else {
                rnonvars.push(r.clone());
            }
            rexprs.push(r);
        }
        let mut result: Option<Node> = None;
        if rnonvars.len() > 1 {
            let mut all = vec![lexpr.clone()];
            all.extend(rnonvars.iter().cloned());
            let mut scalar_type = self.select_common_type(&all, None)?;
            if scalar_type != 0 && !self.verify_common_type(scalar_type, &all) {
                scalar_type = 0;
            }
            let array_type = if scalar_type != 0 && scalar_type != RECORDOID {
                self.cat.array_type(scalar_type)
            } else {
                0
            };
            if array_type != 0 {
                let mut aexprs = Vec::new();
                for r in rnonvars {
                    aexprs.push(self.coerce_to_common_type(r, scalar_type, "IN")?);
                }
                let newa = Node::ArrayExpr(Box::new(ArrayExpr {
                    array_typeid: array_type,
                    array_collid: 0,
                    element_typeid: scalar_type,
                    elements: aexprs,
                    multidims: false,
                    location: -1,
                }));
                result = Some(self.make_scalar_array_op(
                    name,
                    use_or,
                    lexpr.clone(),
                    newa,
                    a.location,
                )?);
                rexprs = rvars;
            }
        }
        for r in rexprs {
            let cmp = match (&lexpr, r) {
                (Node::RowExpr(lr), Node::RowExpr(rr)) => {
                    self.make_row_comparison_op(name, lr.args.clone(), rr.args, a.location)?
                }
                (l, r) => self.make_op(name, Some(l.clone()), r, a.location)?,
            };
            let cmp = self.coerce_to_boolean(cmp, "IN")?;
            result = Some(match result {
                None => cmp,
                Some(prev) => Node::BoolExpr(Box::new(BoolExpr {
                    boolop: if use_or { BoolOp::Or } else { BoolOp::And },
                    args: vec![prev, cmp],
                    location: a.location,
                })),
            });
        }
        result.ok_or_else(|| AnalysisError("an empty IN list".into()))
    }

    /// `transformAExprBetween`: rebuilt as the comparisons it stands for,
    /// then transformed.
    fn transform_aexpr_between(&mut self, a: &pb::AExpr, kind: pb::AExprKind) -> R<Node> {
        use pb::AExprKind as K;
        let aexpr = a
            .lexpr
            .as_deref()
            .ok_or_else(|| AnalysisError("BETWEEN".into()))?
            .clone();
        let args: Vec<pb::Node> = match a.rexpr.as_deref().and_then(|r| r.node.as_ref()) {
            Some(N::List(l)) => l.items.clone(),
            _ => return unsupported("BETWEEN's bounds"),
        };
        let [bexpr, cexpr] = <[pb::Node; 2]>::try_from(args)
            .map_err(|_| AnalysisError("BETWEEN's two bounds".into()))?;
        let op = |name: &str, l: &pb::Node, r: &pb::Node| pb::Node {
            node: Some(N::AExpr(Box::new(pb::AExpr {
                kind: K::AexprOp as i32,
                name: vec![super::clause::string_node(name)],
                lexpr: Some(Box::new(l.clone())),
                rexpr: Some(Box::new(r.clone())),
                location: a.location,
            }))),
        };
        let boolexpr = |t: pb::BoolExprType, args: Vec<pb::Node>| pb::Node {
            node: Some(N::BoolExpr(Box::new(pb::BoolExpr {
                xpr: None,
                boolop: t as i32,
                args,
                location: a.location,
            }))),
        };
        use pb::BoolExprType as B;
        let tree = match kind {
            K::AexprBetween => boolexpr(
                B::AndExpr,
                vec![op(">=", &aexpr, &bexpr), op("<=", &aexpr, &cexpr)],
            ),
            K::AexprNotBetween => boolexpr(
                B::OrExpr,
                vec![op("<", &aexpr, &bexpr), op(">", &aexpr, &cexpr)],
            ),
            K::AexprBetweenSym => boolexpr(
                B::OrExpr,
                vec![
                    boolexpr(
                        B::AndExpr,
                        vec![op(">=", &aexpr, &bexpr), op("<=", &aexpr, &cexpr)],
                    ),
                    boolexpr(
                        B::AndExpr,
                        vec![op(">=", &aexpr, &cexpr), op("<=", &aexpr, &bexpr)],
                    ),
                ],
            ),
            _ => boolexpr(
                B::AndExpr,
                vec![
                    boolexpr(
                        B::OrExpr,
                        vec![op("<", &aexpr, &bexpr), op(">", &aexpr, &cexpr)],
                    ),
                    boolexpr(
                        B::OrExpr,
                        vec![op("<", &aexpr, &cexpr), op(">", &aexpr, &bexpr)],
                    ),
                ],
            ),
        };
        self.transform_expr_recurse(&tree)
    }

    /// `transformBoolExpr`.
    fn transform_bool_expr(&mut self, b: &pb::BoolExpr) -> R<Node> {
        let (op, name) = match b.boolop - 1 {
            0 => (BoolOp::And, "AND"),
            1 => (BoolOp::Or, "OR"),
            _ => (BoolOp::Not, "NOT"),
        };
        let mut args = Vec::new();
        for a in &b.args {
            let e = self.transform_expr_recurse(a)?;
            args.push(self.coerce_to_boolean(e, name)?);
        }
        Ok(Node::BoolExpr(Box::new(BoolExpr {
            boolop: op,
            args,
            location: b.location,
        })))
    }

    /// `transformFuncCall`.
    fn transform_func_call(&mut self, f: &pb::FuncCall) -> R<Node> {
        let names = super::clause::string_list(&f.funcname);
        // PostgreSQL 16 turned these SQLValueFunctions into function calls
        // of SQL syntax; 15's analysis builds the SQLValueFunction.
        if f.funcformat == pb::CoercionForm::CoerceSqlSyntax as i32
            && f.args.is_empty()
            && names.len() == 2
            && names[0] == "pg_catalog"
        {
            let op = match names[1].as_str() {
                "current_role" => Some(9),
                "current_user" => Some(10),
                "user" => Some(11),
                "session_user" => Some(12),
                "current_database" => Some(13),
                "current_schema" => Some(14),
                _ => None,
            };
            if let Some(op) = op {
                return Ok(self.transform_sql_value_function(op, -1, f.location));
            }
            if names[1] == "system_user" {
                return unsupported("SYSTEM_USER is PostgreSQL 16's");
            }
        }
        let mut targs = Vec::new();
        for a in &f.args {
            targs.push(self.transform_expr_recurse(a)?);
        }
        if f.agg_within_group {
            for o in &f.agg_order {
                let Some(N::SortBy(sb)) = o.node.as_ref() else {
                    return unsupported("WITHIN GROUP's ordering");
                };
                let n = sb
                    .node
                    .as_deref()
                    .ok_or_else(|| AnalysisError("an ordering".into()))?;
                targs.push(self.transform_expr(n, ExprKind::OrderBy)?);
            }
        }
        self.parse_func_or_column(&names, targs, Some(f), f.location)?
            .ok_or_else(|| AnalysisError(format!("function {} does not exist", names.join("."))))
    }

    /// `transformSQLValueFunction` (op is PostgreSQL 15's `SQLValueFunctionOp`).
    fn transform_sql_value_function(&mut self, op: i32, typmod: i32, location: i32) -> Node {
        let (op, t) = match op {
            0 => (SvfOp::CurrentDate, DATEOID),
            1 => (SvfOp::CurrentTime, TIMETZOID),
            2 => (SvfOp::CurrentTimeN, TIMETZOID),
            3 => (SvfOp::CurrentTimestamp, TIMESTAMPTZOID),
            4 => (SvfOp::CurrentTimestampN, TIMESTAMPTZOID),
            5 => (SvfOp::Localtime, TIMEOID),
            6 => (SvfOp::LocaltimeN, TIMEOID),
            7 => (SvfOp::Localtimestamp, TIMESTAMPOID),
            8 => (SvfOp::LocaltimestampN, TIMESTAMPOID),
            9 => (SvfOp::CurrentRole, NAMEOID),
            10 => (SvfOp::CurrentUser, NAMEOID),
            11 => (SvfOp::User, NAMEOID),
            12 => (SvfOp::SessionUser, NAMEOID),
            13 => (SvfOp::CurrentCatalog, NAMEOID),
            _ => (SvfOp::CurrentSchema, NAMEOID),
        };
        let typmod = match op {
            SvfOp::CurrentTimeN
            | SvfOp::CurrentTimestampN
            | SvfOp::LocaltimeN
            | SvfOp::LocaltimestampN => typmod.min(6),
            _ => typmod,
        };
        Node::SqlValueFunction(SqlValueFunction {
            op,
            type_: t,
            typmod,
            location,
        })
    }

    /// `transformSubLink`.
    fn transform_sub_link(&mut self, s: &pb::SubLink) -> R<Node> {
        self.ps().has_sub_links = true;
        let sub = s
            .subselect
            .as_deref()
            .ok_or_else(|| AnalysisError("a sublink".into()))?;
        let q = self.sub_analyze(sub, false, true)?;
        if q.command_type != CMD_SELECT {
            return unsupported("a non-SELECT sublink");
        }
        let kind = match pb::SubLinkType::try_from(s.sub_link_type) {
            Ok(pb::SubLinkType::ExistsSublink) => SubLinkType::Exists,
            Ok(pb::SubLinkType::AllSublink) => SubLinkType::All,
            Ok(pb::SubLinkType::AnySublink) => SubLinkType::Any,
            Ok(pb::SubLinkType::RowcompareSublink) => SubLinkType::RowCompare,
            Ok(pb::SubLinkType::ExprSublink) => SubLinkType::Expr,
            Ok(pb::SubLinkType::ArraySublink) => SubLinkType::Array,
            _ => return unsupported("this sublink"),
        };
        let mut link = SubLink {
            sub_link_type: kind,
            sub_link_id: 0,
            testexpr: None,
            oper_name: Vec::new(),
            subselect: Box::new(q),
            location: s.location,
        };
        match kind {
            SubLinkType::Exists => {}
            SubLinkType::Expr | SubLinkType::Array => {
                if link
                    .subselect
                    .target_list
                    .iter()
                    .filter(|t| !t.resjunk)
                    .count()
                    != 1
                {
                    return unsupported("subquery must return only one column");
                }
            }
            _ => {
                let mut oper = super::clause::string_list(&s.oper_name);
                if oper.is_empty() {
                    oper = vec!["=".to_string()];
                }
                let test = s
                    .testexpr
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a sublink test".into()))?;
                let lefthand = self.transform_expr_recurse(test)?;
                let left_list = match lefthand {
                    Node::RowExpr(r) => r.args,
                    other => vec![other],
                };
                let mut right_list = Vec::new();
                for t in &link.subselect.target_list {
                    if t.resjunk {
                        continue;
                    }
                    right_list.push(Node::Param(Param {
                        paramkind: 2,
                        paramid: t.resno,
                        paramtype: expr_type(self.cat, &t.expr),
                        paramtypmod: expr_typmod(self.cat, &t.expr),
                        paramcollid: expr_collation(self.cat, &t.expr),
                        location: -1,
                    }));
                }
                if left_list.len() < right_list.len() {
                    return unsupported("subquery has too many columns");
                }
                if left_list.len() > right_list.len() {
                    return unsupported("subquery has too few columns");
                }
                link.testexpr =
                    Some(self.make_row_comparison_op(&oper, left_list, right_list, s.location)?);
                link.oper_name = oper;
            }
        }
        Ok(Node::SubLink(Box::new(link)))
    }

    /// `transformArrayExpr`.
    fn transform_array_expr(
        &mut self,
        a: &pb::AArrayExpr,
        mut array_type: Oid,
        mut element_type: Oid,
        typmod: i32,
    ) -> R<Node> {
        let mut multidims = false;
        let mut newelems = Vec::new();
        for e in &a.elements {
            if let Some(N::AArrayExpr(sub)) = e.node.as_ref() {
                newelems.push(self.transform_array_expr(sub, array_type, element_type, typmod)?);
                multidims = true;
            } else {
                let ne = self.transform_expr_recurse(e)?;
                if !multidims {
                    let t = expr_type(self.cat, &ne);
                    if t != INT2VECTOROID && t != OIDVECTOROID && self.cat.type_is_array(t) {
                        multidims = true;
                    }
                }
                newelems.push(ne);
            }
        }
        let (coerce_type, coerce_hard) = if array_type != 0 {
            (if multidims { array_type } else { element_type }, true)
        } else {
            if newelems.is_empty() {
                return unsupported("cannot determine type of empty array");
            }
            let t = self.select_common_type(&newelems, Some("ARRAY"))?;
            if multidims {
                array_type = t;
                element_type = self.cat.element_type(array_type);
                if element_type == 0 {
                    return unsupported("could not find element type");
                }
            } else {
                element_type = t;
                array_type = self.cat.array_type(element_type);
                if array_type == 0 {
                    return unsupported("could not find array type");
                }
            }
            (t, false)
        };
        let mut coerced = Vec::new();
        for e in newelems {
            let ne = if coerce_hard {
                let t = expr_type(self.cat, &e);
                self.coerce_to_target_type(
                    e,
                    t,
                    coerce_type,
                    typmod,
                    CoercionContext::Explicit,
                    CoercionForm::ExplicitCast,
                    -1,
                )?
                .ok_or_else(|| AnalysisError("cannot cast an array element".into()))?
            } else {
                self.coerce_to_common_type(e, coerce_type, "ARRAY")?
            };
            coerced.push(ne);
        }
        Ok(Node::ArrayExpr(Box::new(ArrayExpr {
            array_typeid: array_type,
            array_collid: 0,
            element_typeid: element_type,
            elements: coerced,
            multidims,
            location: a.location,
        })))
    }

    /// `transformRowExpr`.
    fn transform_row_expr(&mut self, r: &pb::RowExpr, allow_default: bool) -> R<Node> {
        let kind = self.psr().expr_kind;
        let args = self.transform_expression_list(&r.args, kind, allow_default)?;
        let colnames = (1..=args.len()).map(|i| format!("f{i}")).collect();
        Ok(Node::RowExpr(Box::new(RowExpr {
            args,
            row_typeid: RECORDOID,
            row_format: CoercionForm::ImplicitCast,
            colnames,
            location: r.location,
        })))
    }

    /// `transformCaseExpr`.
    fn transform_case_expr(&mut self, c: &pb::CaseExpr) -> R<Node> {
        let mut arg = match c.arg.as_deref() {
            Some(a) => Some(self.transform_expr_recurse(a)?),
            None => None,
        };
        let placeholder = match arg.as_mut() {
            Some(a) => {
                if expr_type(self.cat, a) == UNKNOWNOID {
                    *a = self.coerce_to_common_type(a.clone(), TEXTOID, "CASE")?;
                }
                self.assign_expr_collations(a)?;
                Some(CaseTestExpr {
                    type_id: expr_type(self.cat, a),
                    type_mod: expr_typmod(self.cat, a),
                    collation: expr_collation(self.cat, a),
                })
            }
            None => None,
        };
        let mut whens = Vec::new();
        for w in &c.args {
            let Some(N::CaseWhen(w)) = w.node.as_ref() else {
                return unsupported("a CASE arm");
            };
            let warg = w
                .expr
                .as_deref()
                .ok_or_else(|| AnalysisError("a WHEN".into()))?;
            let expr = match &placeholder {
                Some(p) => {
                    let r = self.transform_expr_recurse(warg)?;
                    self.make_op(
                        &["=".to_string()],
                        Some(Node::CaseTestExpr(p.clone())),
                        r,
                        w.location,
                    )?
                }
                None => self.transform_expr_recurse(warg)?,
            };
            let expr = self.coerce_to_boolean(expr, "CASE/WHEN")?;
            let result = self.transform_expr_recurse(
                w.result
                    .as_deref()
                    .ok_or_else(|| AnalysisError("a THEN".into()))?,
            )?;
            whens.push(CaseWhen {
                expr,
                result,
                location: w.location,
            });
        }
        let defresult = match c.defresult.as_deref() {
            Some(d) => self.transform_expr_recurse(d)?,
            None => Node::Const(Const {
                consttype: UNKNOWNOID,
                consttypmod: -1,
                constcollid: 0,
                constlen: -2,
                constbyval: false,
                constvalue: None,
                location: -1,
            }),
        };
        let mut resultexprs = vec![defresult.clone()];
        resultexprs.extend(whens.iter().map(|w| w.result.clone()));
        let ptype = self.select_common_type(&resultexprs, Some("CASE"))?;
        let defresult = self.coerce_to_common_type(defresult, ptype, "CASE/ELSE")?;
        for w in whens.iter_mut() {
            let r = w.result.clone();
            w.result = self.coerce_to_common_type(r, ptype, "CASE/WHEN")?;
        }
        Ok(Node::CaseExpr(Box::new(CaseExpr {
            casetype: ptype,
            casecollid: 0,
            arg,
            args: whens,
            defresult,
            location: c.location,
        })))
    }

    /// `transformCoalesceExpr`.
    fn transform_coalesce_expr(&mut self, c: &pb::CoalesceExpr) -> R<Node> {
        let mut args = Vec::new();
        for a in &c.args {
            args.push(self.transform_expr_recurse(a)?);
        }
        let t = self.select_common_type(&args, Some("COALESCE"))?;
        let mut coerced = Vec::new();
        for a in args {
            coerced.push(self.coerce_to_common_type(a, t, "COALESCE")?);
        }
        Ok(Node::CoalesceExpr(Box::new(CoalesceExpr {
            coalescetype: t,
            coalescecollid: 0,
            args: coerced,
            location: c.location,
        })))
    }

    /// `transformMinMaxExpr`.
    fn transform_min_max_expr(&mut self, m: &pb::MinMaxExpr) -> R<Node> {
        let op = m.op - 1;
        let name = if op == 0 { "GREATEST" } else { "LEAST" };
        let mut args = Vec::new();
        for a in &m.args {
            args.push(self.transform_expr_recurse(a)?);
        }
        let t = self.select_common_type(&args, Some(name))?;
        let mut coerced = Vec::new();
        for a in args {
            coerced.push(self.coerce_to_common_type(a, t, name)?);
        }
        Ok(Node::MinMaxExpr(Box::new(MinMaxExpr {
            minmaxtype: t,
            minmaxcollid: 0,
            inputcollid: 0,
            op,
            args: coerced,
            location: m.location,
        })))
    }

    /// `transformTypeCast`.
    fn transform_type_cast(&mut self, tc: &pb::TypeCast) -> R<Node> {
        let tn = tc
            .type_name
            .as_ref()
            .ok_or_else(|| AnalysisError("a cast without a type".into()))?;
        let (target, typmod) = self.typename_type_id_and_mod(tn)?;
        let arg = tc
            .arg
            .as_deref()
            .ok_or_else(|| AnalysisError("a cast without an argument".into()))?;
        let expr = if let Some(N::AArrayExpr(ae)) = arg.node.as_ref() {
            let mut basemod = typmod;
            let base = self.cat.base_type_and_typmod(target, &mut basemod);
            let elem = self.cat.element_type(base);
            if elem != 0 {
                self.transform_array_expr(ae, base, elem, basemod)?
            } else {
                self.transform_expr_recurse(arg)?
            }
        } else {
            self.transform_expr_recurse(arg)?
        };
        let input = expr_type(self.cat, &expr);
        if input == 0 {
            return Ok(expr);
        }
        let location = if tc.location >= 0 {
            tc.location
        } else {
            tn.location
        };
        self.coerce_to_target_type(
            expr,
            input,
            target,
            typmod,
            CoercionContext::Explicit,
            CoercionForm::ExplicitCast,
            location,
        )?
        .ok_or_else(|| AnalysisError("cannot cast".into()))
    }

    /// `transformCollateClause`.
    fn transform_collate_clause(&mut self, c: &pb::CollateClause) -> R<Node> {
        let arg = self.transform_expr_recurse(
            c.arg
                .as_deref()
                .ok_or_else(|| AnalysisError("COLLATE".into()))?,
        )?;
        let t = expr_type(self.cat, &arg);
        if !self.cat.type_is_collatable(t) && t != UNKNOWNOID {
            return unsupported("collations are not supported by this type");
        }
        let coll = self.lookup_collation(&c.collname)?;
        Ok(Node::CollateExpr(Box::new(CollateExpr {
            arg,
            coll_oid: coll,
            location: c.location,
        })))
    }

    /// `LookupCollation`.
    pub fn lookup_collation(&self, names: &[pb::Node]) -> R<Oid> {
        let names = super::clause::string_list(names);
        let (nsp, name) = match names.as_slice() {
            [n] => (None, n.clone()),
            [s, n] => (Some(s.clone()), n.clone()),
            _ => return unsupported("a collation name"),
        };
        let enc = self.cat.data.encoding;
        let matches = |c: &CollRow| c.name == name && (c.encoding == enc || c.encoding == -1);
        if let Some(s) = nsp {
            let nspoid = self
                .cat
                .namespace_oid(&s)
                .ok_or_else(|| AnalysisError(format!("schema \"{s}\" does not exist")))?;
            return self
                .cat
                .data
                .collations
                .iter()
                .filter(|c| c.nsp == nspoid && matches(c))
                .min_by_key(|c| if c.encoding == enc { 0 } else { 1 })
                .map(|c| c.oid)
                .ok_or_else(|| AnalysisError(format!("collation \"{name}\" does not exist")));
        }
        for n in &self.path {
            if let Some(c) = self
                .cat
                .data
                .collations
                .iter()
                .filter(|c| c.nsp == *n && matches(c))
                .min_by_key(|c| if c.encoding == enc { 0 } else { 1 })
            {
                return Ok(c.oid);
            }
        }
        unsupported(format!("collation \"{name}\" does not exist"))
    }

    /// `make_row_comparison_op`.
    pub fn make_row_comparison_op(
        &mut self,
        opname: &[String],
        largs: Vec<Node>,
        rargs: Vec<Node>,
        location: i32,
    ) -> R<Node> {
        let n = largs.len();
        if n != rargs.len() {
            return unsupported("unequal number of entries in row expressions");
        }
        if n == 0 {
            return unsupported("cannot compare rows of zero length");
        }
        let mut opexprs = Vec::new();
        for (l, r) in largs.into_iter().zip(rargs) {
            let cmp = self.make_op(opname, Some(l), r, location)?;
            let Node::OpExpr(o) = &cmp else {
                return unsupported("a row comparison operator");
            };
            if o.opresulttype != BOOLOID {
                return unsupported("row comparison operator must yield type boolean");
            }
            opexprs.push(cmp);
        }
        if n == 1 {
            return Ok(opexprs.pop().expect("one"));
        }
        let mut infos = Vec::new();
        let mut strats: Option<std::collections::BTreeSet<i32>> = None;
        for e in &opexprs {
            let Node::OpExpr(o) = e else { unreachable!() };
            let info = self.cat.op_btree_interpretation(o.opno);
            let this: std::collections::BTreeSet<i32> = info.iter().map(|(_, s)| *s).collect();
            strats = Some(match strats {
                None => this,
                Some(s) => s.intersection(&this).copied().collect(),
            });
            infos.push(info);
        }
        let Some(rctype) = strats.and_then(|s| s.into_iter().next()) else {
            return unsupported("could not determine interpretation of row comparison operator");
        };
        if rctype == 3 {
            return Ok(Node::BoolExpr(Box::new(BoolExpr {
                boolop: BoolOp::And,
                args: opexprs,
                location,
            })));
        }
        if rctype == 6 {
            return Ok(Node::BoolExpr(Box::new(BoolExpr {
                boolop: BoolOp::Or,
                args: opexprs,
                location,
            })));
        }
        let mut opfamilies = Vec::new();
        for info in &infos {
            let f = info
                .iter()
                .find(|(_, s)| *s == rctype)
                .map(|(f, _)| *f)
                .ok_or_else(|| AnalysisError("row comparison operator".into()))?;
            opfamilies.push(f);
        }
        let mut opnos = Vec::new();
        let mut l = Vec::new();
        let mut r = Vec::new();
        for e in opexprs {
            let Node::OpExpr(o) = e else { unreachable!() };
            opnos.push(o.opno);
            let mut it = o.args.into_iter();
            l.push(it.next().expect("left"));
            r.push(it.next().expect("right"));
        }
        Ok(Node::RowCompareExpr(Box::new(RowCompareExpr {
            rctype,
            opnos,
            opfamilies,
            inputcollids: Vec::new(),
            largs: l,
            rargs: r,
        })))
    }
}

pub(crate) fn bool_const(v: bool) -> Node {
    Node::Const(Const {
        consttype: BOOLOID,
        consttypmod: -1,
        constcollid: 0,
        constlen: 1,
        constbyval: true,
        constvalue: Some(Datum::from_i64(i64::from(v), 1)),
        location: -1,
    })
}
