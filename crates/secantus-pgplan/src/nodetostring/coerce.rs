//! `parse_coerce.c`.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_type, expr_typmod};
use super::nodes::*;
use super::parse::Analyzer;
use super::{pg_error, unsupported, AnalysisError, R};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoercionPath {
    None,
    Func,
    Relabel,
    ArrayCoerce,
    CoerceViaIo,
}

pub(crate) fn is_polymorphic_family1(t: Oid) -> bool {
    matches!(
        t,
        ANYELEMENTOID | ANYARRAYOID | ANYNONARRAYOID | ANYENUMOID | ANYRANGEOID | ANYMULTIRANGEOID
    )
}

pub(crate) fn is_polymorphic_family2(t: Oid) -> bool {
    matches!(
        t,
        ANYCOMPATIBLEOID
            | ANYCOMPATIBLEARRAYOID
            | ANYCOMPATIBLENONARRAYOID
            | ANYCOMPATIBLERANGEOID
            | ANYCOMPATIBLEMULTIRANGEOID
    )
}

pub(crate) fn is_polymorphic(t: Oid) -> bool {
    is_polymorphic_family1(t) || is_polymorphic_family2(t)
}

/// `IsBinaryCoercible`.
pub(crate) fn is_binary_coercible(cat: &Catalog, src: Oid, target: Oid) -> bool {
    if src == target {
        return true;
    }
    if target == ANYOID || target == ANYELEMENTOID || target == ANYCOMPATIBLEOID {
        return true;
    }
    let src = if src != 0 { cat.base_type(src) } else { src };
    if src == target {
        return true;
    }
    if (target == ANYARRAYOID || target == ANYCOMPATIBLEARRAYOID) && cat.type_is_array(src) {
        return true;
    }
    if (target == ANYNONARRAYOID || target == ANYCOMPATIBLENONARRAYOID) && !cat.type_is_array(src) {
        return true;
    }
    if target == ANYENUMOID && cat.type_is_enum(src) {
        return true;
    }
    if (target == ANYRANGEOID || target == ANYCOMPATIBLERANGEOID) && cat.type_is_range(src) {
        return true;
    }
    if (target == ANYMULTIRANGEOID || target == ANYCOMPATIBLEMULTIRANGEOID)
        && cat.type_is_multirange(src)
    {
        return true;
    }
    if target == RECORDOID && cat.is_complex(src) {
        return true;
    }
    if target == RECORDARRAYOID && is_complex_array(cat, src) {
        return true;
    }
    match cat.cast(src, target) {
        Some(c) => c.method == 'b' && c.context == 'i',
        None => false,
    }
}

fn is_complex_array(cat: &Catalog, t: Oid) -> bool {
    let e = cat.element_type(t);
    e != 0 && cat.is_complex(e)
}

impl<'a> Analyzer<'a> {
    /// `find_coercion_pathway`.
    pub fn find_coercion_pathway(
        &self,
        target: Oid,
        source: Oid,
        ccontext: CoercionContext,
    ) -> (CoercionPath, Oid) {
        let source = if source != 0 {
            self.cat.base_type(source)
        } else {
            source
        };
        let target = if target != 0 {
            self.cat.base_type(target)
        } else {
            target
        };
        if source == target {
            return (CoercionPath::Relabel, 0);
        }
        if source == INTERNALOID || target == INTERNALOID {
            return (CoercionPath::None, 0);
        }
        let mut result = (CoercionPath::None, 0);
        if let Some(c) = self.cat.cast(source, target) {
            let castcontext = match c.context {
                'i' => CoercionContext::Implicit,
                'a' => CoercionContext::Assignment,
                _ => CoercionContext::Explicit,
            };
            if ccontext >= castcontext {
                result = match c.method {
                    'f' => (CoercionPath::Func, c.func),
                    'i' => (CoercionPath::CoerceViaIo, 0),
                    _ => (CoercionPath::Relabel, 0),
                };
            }
        } else {
            if target != OIDVECTOROID && target != INT2VECTOROID {
                let te = self.cat.element_type(target);
                let se = self.cat.element_type(source);
                if te != 0
                    && se != 0
                    && self.find_coercion_pathway(te, se, ccontext).0 != CoercionPath::None
                {
                    result = (CoercionPath::ArrayCoerce, 0);
                }
            }
            if result.0 == CoercionPath::None {
                if ccontext >= CoercionContext::Assignment && self.cat.type_category(target) == 'S'
                {
                    result = (CoercionPath::CoerceViaIo, 0);
                } else if ccontext >= CoercionContext::Explicit
                    && self.cat.type_category(source) == 'S'
                {
                    result = (CoercionPath::CoerceViaIo, 0);
                }
            }
        }
        if result.0 == CoercionPath::None && ccontext == CoercionContext::Plpgsql {
            result = (CoercionPath::CoerceViaIo, 0);
        }
        result
    }

    /// `find_typmod_coercion_function`.
    fn find_typmod_coercion_function(&self, mut t: Oid) -> (CoercionPath, Oid) {
        let mut result = CoercionPath::Func;
        if let Some(r) = self.cat.type_row(t) {
            if r.len == -1 && r.subscript == F_ARRAY_SUBSCRIPT_HANDLER && r.elem != 0 {
                t = r.elem;
                result = CoercionPath::ArrayCoerce;
            }
        }
        match self.cat.cast(t, t) {
            Some(c) if c.func != 0 => (result, c.func),
            _ => (CoercionPath::None, 0),
        }
    }

    /// `can_coerce_type`.
    pub fn can_coerce_type(
        &self,
        input: &[Oid],
        target: &[Oid],
        ccontext: CoercionContext,
    ) -> bool {
        let mut have_generics = false;
        for (i, &it) in input.iter().enumerate() {
            let tt = target[i];
            if it == tt {
                continue;
            }
            if it == INTERNALOID || tt == INTERNALOID {
                return false;
            }
            if tt == ANYOID {
                continue;
            }
            if is_polymorphic(tt) {
                have_generics = true;
                continue;
            }
            if it == UNKNOWNOID {
                continue;
            }
            if self.find_coercion_pathway(tt, it, ccontext).0 != CoercionPath::None {
                continue;
            }
            if it == RECORDOID && self.cat.is_complex(tt) {
                continue;
            }
            if tt == RECORDOID && self.cat.is_complex(it) {
                continue;
            }
            if tt == RECORDARRAYOID && is_complex_array(self.cat, it) {
                continue;
            }
            if self.cat.type_inherits_from(it, tt) || self.cat.type_is_of_typed_table(it, tt) {
                continue;
            }
            return false;
        }
        if have_generics && !self.check_generic_type_consistency(input, target) {
            return false;
        }
        true
    }

    /// `coerce_to_target_type`.
    #[allow(clippy::too_many_arguments)]
    pub fn coerce_to_target_type(
        &mut self,
        expr: Node,
        exprtype: Oid,
        targettype: Oid,
        targettypmod: i32,
        ccontext: CoercionContext,
        cformat: CoercionForm,
        location: i32,
    ) -> R<Option<Node>> {
        if !self.can_coerce_type(&[exprtype], &[targettype], ccontext) {
            return Ok(None);
        }
        let mut origcoll: Option<(Oid, i32)> = None;
        let mut inner = expr;
        while let Node::CollateExpr(c) = inner {
            if origcoll.is_none() {
                origcoll = Some((c.coll_oid, c.location));
            }
            inner = c.arg;
        }
        let inner_before = inner.clone();
        let result = self.coerce_type(
            inner,
            exprtype,
            targettype,
            targettypmod,
            ccontext,
            cformat,
            location,
        )?;
        let changed = result != inner_before && !matches!(result, Node::Const(_));
        let result = self.coerce_type_typmod(
            result,
            targettype,
            targettypmod,
            ccontext,
            cformat,
            location,
            changed,
        )?;
        if let Some((coll, loc)) = origcoll {
            if self.cat.type_is_collatable(targettype) {
                return Ok(Some(Node::CollateExpr(Box::new(CollateExpr {
                    arg: result,
                    coll_oid: coll,
                    location: loc,
                }))));
            }
        }
        Ok(Some(result))
    }

    /// `coerce_type`.
    #[allow(clippy::too_many_arguments)]
    pub fn coerce_type(
        &mut self,
        node: Node,
        input: Oid,
        target: Oid,
        targettypmod: i32,
        ccontext: CoercionContext,
        cformat: CoercionForm,
        location: i32,
    ) -> R<Node> {
        if target == input {
            return Ok(node);
        }
        if matches!(
            target,
            ANYOID | ANYELEMENTOID | ANYNONARRAYOID | ANYCOMPATIBLEOID | ANYCOMPATIBLENONARRAYOID
        ) {
            return Ok(node);
        }
        if matches!(
            target,
            ANYARRAYOID
                | ANYENUMOID
                | ANYRANGEOID
                | ANYMULTIRANGEOID
                | ANYCOMPATIBLEARRAYOID
                | ANYCOMPATIBLERANGEOID
                | ANYCOMPATIBLEMULTIRANGEOID
        ) && input != UNKNOWNOID
        {
            let base = self.cat.base_type(input);
            if base != input {
                return Ok(Node::RelabelType(Box::new(RelabelType {
                    arg: node,
                    resulttype: base,
                    resulttypmod: -1,
                    resultcollid: 0,
                    relabelformat: cformat,
                    location,
                })));
            }
            return Ok(node);
        }
        if input == UNKNOWNOID {
            if let Node::Const(con) = &node {
                let mut basemod = targettypmod;
                let base = self.cat.base_type_and_typmod(target, &mut basemod);
                let inputmod = if base == INTERVALOID { basemod } else { -1 };
                let row = self
                    .cat
                    .type_row(base)
                    .ok_or_else(|| AnalysisError::new(format!("type {base} does not exist")))?
                    .clone();
                let value = match &con.constvalue {
                    None => None,
                    Some(Datum::ByRef(bytes)) => {
                        let s = std::str::from_utf8(&bytes[..bytes.len().saturating_sub(1)])
                            .map_err(|_| AnalysisError::new("a literal's encoding".into()))?;
                        Some(super::datum::type_input(self, base, s, inputmod)?)
                    }
                    Some(_) => return unsupported("an unknown literal's value"),
                };
                let newcon = Node::Const(Const {
                    consttype: base,
                    consttypmod: inputmod,
                    constcollid: row.collation,
                    constlen: row.len.into(),
                    constbyval: row.byval,
                    constvalue: value,
                    location: con.location,
                });
                if base != target {
                    return self.coerce_to_domain(
                        newcon, base, basemod, target, ccontext, cformat, location, false,
                    );
                }
                return Ok(newcon);
            }
        }
        if let Node::CollateExpr(c) = node {
            let coll = c.coll_oid;
            let loc = c.location;
            let r = self.coerce_type(
                c.arg,
                input,
                target,
                targettypmod,
                ccontext,
                cformat,
                location,
            )?;
            if self.cat.type_is_collatable(target) {
                return Ok(Node::CollateExpr(Box::new(CollateExpr {
                    arg: r,
                    coll_oid: coll,
                    location: loc,
                })));
            }
            return Ok(r);
        }
        let (path, funcid) = self.find_coercion_pathway(target, input, ccontext);
        if path != CoercionPath::None {
            if path != CoercionPath::Relabel {
                let mut basemod = targettypmod;
                let base = self.cat.base_type_and_typmod(target, &mut basemod);
                let result = self.build_coercion_expression(
                    node, path, funcid, base, basemod, ccontext, cformat, location,
                )?;
                if target != base {
                    return self.coerce_to_domain(
                        result, base, basemod, target, ccontext, cformat, location, true,
                    );
                }
                return Ok(result);
            }
            let before = node.clone();
            let result =
                self.coerce_to_domain(node, 0, -1, target, ccontext, cformat, location, false)?;
            if result == before {
                return Ok(Node::RelabelType(Box::new(RelabelType {
                    arg: result,
                    resulttype: target,
                    resulttypmod: -1,
                    resultcollid: 0,
                    relabelformat: cformat,
                    location,
                })));
            }
            return Ok(result);
        }
        if input == RECORDOID && self.cat.is_complex(target) {
            return unsupported("coercing a RECORD to a composite type in a stored rule");
        }
        if target == RECORDOID && self.cat.is_complex(input) {
            return Ok(node);
        }
        if target == RECORDARRAYOID && is_complex_array(self.cat, input) {
            return Ok(node);
        }
        if self.cat.type_inherits_from(input, target)
            || self.cat.type_is_of_typed_table(input, target)
        {
            let base = self.cat.base_type(input);
            let arg = if base != input {
                Node::RelabelType(Box::new(RelabelType {
                    arg: node,
                    resulttype: base,
                    resulttypmod: -1,
                    resultcollid: 0,
                    relabelformat: CoercionForm::ImplicitCast,
                    location,
                }))
            } else {
                node
            };
            return Ok(Node::ConvertRowtypeExpr(Box::new(ConvertRowtypeExpr {
                arg,
                resulttype: target,
                convertformat: cformat,
                location,
            })));
        }
        unsupported(format!(
            "failed to find conversion function from {input} to {target}"
        ))
    }

    /// `coerce_to_domain`.
    #[allow(clippy::too_many_arguments)]
    pub fn coerce_to_domain(
        &mut self,
        arg: Node,
        mut base: Oid,
        mut basemod: i32,
        typid: Oid,
        ccontext: CoercionContext,
        cformat: CoercionForm,
        location: i32,
        hide_input: bool,
    ) -> R<Node> {
        if base == 0 {
            basemod = -1;
            base = self.cat.base_type_and_typmod(typid, &mut basemod);
        }
        if base == typid {
            return Ok(arg);
        }
        let mut arg = arg;
        if hide_input {
            hide_coercion_node(&mut arg)?;
        }
        let arg = self.coerce_type_typmod(
            arg,
            base,
            basemod,
            ccontext,
            CoercionForm::ImplicitCast,
            location,
            false,
        )?;
        Ok(Node::CoerceToDomain(Box::new(CoerceToDomain {
            arg,
            resulttype: typid,
            resulttypmod: -1,
            resultcollid: 0,
            coercionformat: cformat,
            location,
        })))
    }

    /// `coerce_type_typmod`.
    #[allow(clippy::too_many_arguments)]
    fn coerce_type_typmod(
        &mut self,
        node: Node,
        target: Oid,
        targettypmod: i32,
        ccontext: CoercionContext,
        cformat: CoercionForm,
        location: i32,
        hide_input: bool,
    ) -> R<Node> {
        if targettypmod == expr_typmod(self.cat, &node) {
            return Ok(node);
        }
        let mut node = node;
        if hide_input {
            hide_coercion_node(&mut node)?;
        }
        let (path, funcid) = if targettypmod < 0 {
            (CoercionPath::None, 0)
        } else {
            self.find_typmod_coercion_function(target)
        };
        if path != CoercionPath::None {
            return self.build_coercion_expression(
                node,
                path,
                funcid,
                target,
                targettypmod,
                ccontext,
                cformat,
                location,
            );
        }
        let coll = expr_collation(self.cat, &node);
        Ok(apply_relabel_type(
            self.cat,
            node,
            target,
            targettypmod,
            coll,
            cformat,
            location,
        ))
    }

    /// `build_coercion_expression`.
    #[allow(clippy::too_many_arguments)]
    fn build_coercion_expression(
        &mut self,
        node: Node,
        path: CoercionPath,
        funcid: Oid,
        target: Oid,
        targettypmod: i32,
        ccontext: CoercionContext,
        cformat: CoercionForm,
        location: i32,
    ) -> R<Node> {
        let nargs = if funcid != 0 {
            self.cat
                .proc_row(funcid)
                .ok_or_else(|| {
                    AnalysisError::new(format!("cache lookup failed for function {funcid}"))
                })?
                .argtypes
                .len()
        } else {
            0
        };
        match path {
            CoercionPath::Func => {
                let mut args = vec![node];
                if nargs >= 2 {
                    args.push(Node::Const(Const {
                        consttype: INT4OID,
                        consttypmod: -1,
                        constcollid: 0,
                        constlen: 4,
                        constbyval: true,
                        constvalue: Some(Datum::from_i64(targettypmod.into(), 4)),
                        location: -1,
                    }));
                }
                if nargs == 3 {
                    args.push(Node::Const(Const {
                        consttype: BOOLOID,
                        consttypmod: -1,
                        constcollid: 0,
                        constlen: 1,
                        constbyval: true,
                        constvalue: Some(Datum::from_i64(
                            i64::from(ccontext == CoercionContext::Explicit),
                            1,
                        )),
                        location: -1,
                    }));
                }
                Ok(Node::FuncExpr(Box::new(FuncExpr {
                    funcid,
                    funcresulttype: target,
                    funcretset: false,
                    funcvariadic: false,
                    funcformat: cformat,
                    funccollid: 0,
                    inputcollid: 0,
                    args,
                    location,
                })))
            }
            CoercionPath::ArrayCoerce => {
                let mut srcmod = expr_typmod(self.cat, &node);
                let srcbase = self
                    .cat
                    .base_type_and_typmod(expr_type(self.cat, &node), &mut srcmod);
                let ctest = Node::CaseTestExpr(CaseTestExpr {
                    type_id: self.cat.element_type(srcbase),
                    type_mod: srcmod,
                    collation: 0,
                });
                let te = self.cat.element_type(target);
                let ct = self.cat.element_type(srcbase);
                let elemexpr = self
                    .coerce_to_target_type(
                        ctest,
                        ct,
                        te,
                        targettypmod,
                        ccontext,
                        cformat,
                        location,
                    )?
                    .ok_or_else(|| {
                        AnalysisError::new("failed to coerce array element type as expected".into())
                    })?;
                let resulttypmod = expr_typmod(self.cat, &elemexpr);
                Ok(Node::ArrayCoerceExpr(Box::new(ArrayCoerceExpr {
                    arg: node,
                    elemexpr,
                    resulttype: target,
                    resulttypmod,
                    resultcollid: 0,
                    coerceformat: cformat,
                    location,
                })))
            }
            CoercionPath::CoerceViaIo => Ok(Node::CoerceViaIO(Box::new(CoerceViaIO {
                arg: node,
                resulttype: target,
                resultcollid: 0,
                coerceformat: cformat,
                location,
            }))),
            _ => unsupported("an unsupported coercion path"),
        }
    }

    /// `coerce_to_boolean`.
    pub fn coerce_to_boolean(&mut self, node: Node, construct: &str) -> R<Node> {
        let t = expr_type(self.cat, &node);
        let node = if t != BOOLOID {
            self.coerce_to_target_type(
                node,
                t,
                BOOLOID,
                -1,
                CoercionContext::Assignment,
                CoercionForm::ImplicitCast,
                -1,
            )?
            .ok_or_else(|| {
                AnalysisError::new(format!("argument of {construct} must be type boolean"))
            })?
        } else {
            node
        };
        if super::nodefuncs::expression_returns_set(&node) {
            return unsupported(format!("argument of {construct} must not return a set"));
        }
        Ok(node)
    }

    /// `coerce_to_specific_type`.
    pub fn coerce_to_specific_type(&mut self, node: Node, target: Oid, construct: &str) -> R<Node> {
        let t = expr_type(self.cat, &node);
        let node = if t != target {
            self.coerce_to_target_type(
                node,
                t,
                target,
                -1,
                CoercionContext::Assignment,
                CoercionForm::ImplicitCast,
                -1,
            )?
            .ok_or_else(|| {
                AnalysisError::new(format!("argument of {construct} has the wrong type"))
            })?
        } else {
            node
        };
        if super::nodefuncs::expression_returns_set(&node) {
            return unsupported(format!("argument of {construct} must not return a set"));
        }
        Ok(node)
    }

    /// `select_common_type`.
    pub fn select_common_type(&self, exprs: &[Node], context: Option<&str>) -> R<Oid> {
        Ok(self.select_common_type_which(exprs, context)?.0)
    }

    /// `select_common_type` with `which_expr`: (type, index of the
    /// expression it came from).
    pub fn select_common_type_which(
        &self,
        exprs: &[Node],
        context: Option<&str>,
    ) -> R<(Oid, usize)> {
        let mut pidx = 0;
        let mut ptype = expr_type(self.cat, &exprs[0]);
        let mut start = 1;
        if ptype != UNKNOWNOID {
            let mut all = true;
            for (i, e) in exprs.iter().enumerate().skip(1) {
                if expr_type(self.cat, e) != ptype {
                    all = false;
                    start = i;
                    break;
                }
            }
            if all {
                return Ok((ptype, 0));
            }
        }
        ptype = self.cat.base_type(ptype);
        let (mut pcat, mut ppref) = self.cat.category_preferred(ptype);
        for (i, e) in exprs.iter().enumerate().skip(start) {
            let ntype = self.cat.base_type(expr_type(self.cat, e));
            if ntype != UNKNOWNOID && ntype != ptype {
                let (ncat, npref) = self.cat.category_preferred(ntype);
                if ptype == UNKNOWNOID {
                    pidx = i;
                    ptype = ntype;
                    pcat = ncat;
                    ppref = npref;
                } else if ncat != pcat {
                    let Some(context) = context else {
                        return Ok((0, pidx));
                    };
                    return pg_error(
                        "42804",
                        format!(
                            "{context} types {} and {} cannot be matched",
                            self.cat.format_type_be(ptype),
                            self.cat.format_type_be(ntype)
                        ),
                    );
                } else if !ppref
                    && self.can_coerce_type(&[ptype], &[ntype], CoercionContext::Implicit)
                    && !self.can_coerce_type(&[ntype], &[ptype], CoercionContext::Implicit)
                {
                    pidx = i;
                    ptype = ntype;
                    pcat = ncat;
                    ppref = npref;
                }
            }
        }
        if ptype == UNKNOWNOID {
            ptype = TEXTOID;
        }
        Ok((ptype, pidx))
    }

    /// `select_common_type_from_oids`.
    fn select_common_type_from_oids(&self, types: &[Oid], noerror: bool) -> R<Oid> {
        let mut ptype = types[0];
        let mut i = 1;
        if ptype != UNKNOWNOID {
            while i < types.len() && types[i] == ptype {
                i += 1;
            }
            if i == types.len() {
                return Ok(ptype);
            }
        }
        ptype = self.cat.base_type(ptype);
        let (mut pcat, mut ppref) = self.cat.category_preferred(ptype);
        for &t in &types[i..] {
            let ntype = self.cat.base_type(t);
            if ntype != UNKNOWNOID && ntype != ptype {
                let (ncat, npref) = self.cat.category_preferred(ntype);
                if ptype == UNKNOWNOID {
                    ptype = ntype;
                    pcat = ncat;
                    ppref = npref;
                } else if ncat != pcat {
                    if noerror {
                        return Ok(0);
                    }
                    return pg_error(
                        "42804",
                        format!(
                            "argument types {} and {} cannot be matched",
                            self.cat.format_type_be(ptype),
                            self.cat.format_type_be(ntype)
                        ),
                    );
                } else if !ppref
                    && self.can_coerce_type(&[ptype], &[ntype], CoercionContext::Implicit)
                    && !self.can_coerce_type(&[ntype], &[ptype], CoercionContext::Implicit)
                {
                    ptype = ntype;
                    pcat = ncat;
                    ppref = npref;
                }
            }
        }
        if ptype == UNKNOWNOID {
            ptype = TEXTOID;
        }
        Ok(ptype)
    }

    /// `verify_common_type`.
    pub fn verify_common_type(&self, t: Oid, exprs: &[Node]) -> bool {
        exprs.iter().all(|e| {
            self.can_coerce_type(&[expr_type(self.cat, e)], &[t], CoercionContext::Implicit)
        })
    }

    /// `coerce_to_common_type`.
    pub fn coerce_to_common_type(&mut self, node: Node, target: Oid, context: &str) -> R<Node> {
        let input = expr_type(self.cat, &node);
        if input == target {
            return Ok(node);
        }
        if self.can_coerce_type(&[input], &[target], CoercionContext::Implicit) {
            self.coerce_type(
                node,
                input,
                target,
                -1,
                CoercionContext::Implicit,
                CoercionForm::ImplicitCast,
                -1,
            )
        } else {
            pg_error(
                "42846",
                format!(
                    "{context} could not convert type {} to {}",
                    self.cat.format_type_be(input),
                    self.cat.format_type_be(target)
                ),
            )
        }
    }

    /// `select_common_typmod`.
    pub fn select_common_typmod(&self, exprs: &[Node], common: Oid) -> i32 {
        let mut result = -1;
        for (i, e) in exprs.iter().enumerate() {
            if expr_type(self.cat, e) != common {
                return -1;
            }
            let m = expr_typmod(self.cat, e);
            if i == 0 {
                result = m;
            } else if result != m {
                return -1;
            }
        }
        result
    }

    /// `check_generic_type_consistency`.
    fn check_generic_type_consistency(&self, actual: &[Oid], declared: &[Oid]) -> bool {
        let mut elem = 0;
        let mut array = 0;
        let mut range = 0;
        let mut multirange = 0;
        let mut ac_range = 0;
        let mut ac_range_elem = 0;
        let mut ac_mrange = 0;
        let mut ac_mrange_elem = 0;
        let mut have_anynonarray = false;
        let mut have_anyenum = false;
        let mut have_ac_nonarray = false;
        let mut ac_types = Vec::new();
        for (j, &decl) in declared.iter().enumerate() {
            let mut a = actual[j];
            match decl {
                ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID => {
                    if decl == ANYNONARRAYOID {
                        have_anynonarray = true;
                    } else if decl == ANYENUMOID {
                        have_anyenum = true;
                    }
                    if a == UNKNOWNOID {
                        continue;
                    }
                    if elem != 0 && a != elem {
                        return false;
                    }
                    elem = a;
                }
                ANYARRAYOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if array != 0 && a != array {
                        return false;
                    }
                    array = a;
                }
                ANYRANGEOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if range != 0 && a != range {
                        return false;
                    }
                    range = a;
                }
                ANYMULTIRANGEOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if multirange != 0 && a != multirange {
                        return false;
                    }
                    multirange = a;
                }
                ANYCOMPATIBLEOID | ANYCOMPATIBLENONARRAYOID => {
                    if decl == ANYCOMPATIBLENONARRAYOID {
                        have_ac_nonarray = true;
                    }
                    if a == UNKNOWNOID {
                        continue;
                    }
                    ac_types.push(a);
                }
                ANYCOMPATIBLEARRAYOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    let e = self.cat.element_type(a);
                    if e == 0 {
                        return false;
                    }
                    ac_types.push(e);
                }
                ANYCOMPATIBLERANGEOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if ac_range != 0 {
                        if ac_range != a {
                            return false;
                        }
                    } else {
                        ac_range = a;
                        ac_range_elem = self.cat.range_subtype(a);
                        if ac_range_elem == 0 {
                            return false;
                        }
                        ac_types.push(ac_range_elem);
                    }
                }
                ANYCOMPATIBLEMULTIRANGEOID => {
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if ac_mrange != 0 {
                        if ac_mrange != a {
                            return false;
                        }
                    } else {
                        ac_mrange = a;
                        ac_mrange_elem = self.cat.multirange_range(a);
                        if ac_mrange_elem == 0 {
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
        if array != 0 && array != ANYARRAYOID {
            let ae = self.cat.element_type(array);
            if ae == 0 {
                return false;
            }
            if elem == 0 {
                elem = ae;
            } else if ae != elem {
                return false;
            }
        }
        if multirange != 0 {
            let mr = self.cat.multirange_range(multirange);
            if mr == 0 {
                return false;
            }
            if range == 0 {
                range = mr;
            } else if mr != range {
                return false;
            }
        }
        if range != 0 {
            let re = self.cat.range_subtype(range);
            if re == 0 {
                return false;
            }
            if elem == 0 {
                elem = re;
            } else if re != elem {
                return false;
            }
        }
        if have_anynonarray && self.cat.type_is_array_domain(elem) {
            return false;
        }
        if have_anyenum && !self.cat.type_is_enum(elem) {
            return false;
        }
        if ac_mrange != 0 {
            if ac_range != 0 {
                if ac_mrange_elem != ac_range {
                    return false;
                }
            } else {
                ac_range = ac_mrange_elem;
                ac_range_elem = self.cat.range_subtype(ac_range);
                if ac_range_elem == 0 {
                    return false;
                }
                ac_types.push(ac_range_elem);
            }
        }
        if !ac_types.is_empty() {
            let Ok(t) = self.select_common_type_from_oids(&ac_types, true) else {
                return false;
            };
            if t == 0 {
                return false;
            }
            if !ac_types
                .iter()
                .all(|a| self.can_coerce_type(&[*a], &[t], CoercionContext::Implicit))
            {
                return false;
            }
            if have_ac_nonarray && self.cat.type_is_array_domain(t) {
                return false;
            }
            if ac_range_elem != 0 && ac_range_elem != t {
                return false;
            }
        }
        true
    }

    /// `enforce_generic_type_consistency` (allow_poly false).
    pub fn enforce_generic_type_consistency(
        &self,
        actual: &[Oid],
        declared: &mut [Oid],
        rettype: Oid,
        _allow_poly: bool,
    ) -> R<Oid> {
        let mut have_poly_ac = false;
        let mut have_poly_unknowns = false;
        let mut elem = 0;
        let mut array = 0;
        let mut range = 0;
        let mut multirange = 0;
        let mut ac_type = 0;
        let mut ac_array = 0;
        let mut ac_range = 0;
        let mut ac_range_elem = 0;
        let mut ac_mrange = 0;
        let mut ac_mrange_elem = 0;
        let mut have_anynonarray = rettype == ANYNONARRAYOID;
        let mut have_anyenum = rettype == ANYENUMOID;
        let mut have_anymultirange = rettype == ANYMULTIRANGEOID;
        let mut have_ac_nonarray = rettype == ANYCOMPATIBLENONARRAYOID;
        let mut have_ac_array = rettype == ANYCOMPATIBLEARRAYOID;
        let mut have_ac_range = rettype == ANYCOMPATIBLERANGEOID;
        let mut have_ac_mrange = rettype == ANYCOMPATIBLEMULTIRANGEOID;
        let mut n_poly = 0;
        let mut ac_types = Vec::new();
        let n = actual.len().min(declared.len());
        for j in 0..n {
            let decl = declared[j];
            let mut a = actual[j];
            match decl {
                ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID => {
                    n_poly += 1;
                    if decl == ANYNONARRAYOID {
                        have_anynonarray = true;
                    } else if decl == ANYENUMOID {
                        have_anyenum = true;
                    }
                    if a == UNKNOWNOID {
                        have_poly_unknowns = true;
                        continue;
                    }
                    if elem != 0 && a != elem {
                        return unsupported("arguments declared \"anyelement\" are not all alike");
                    }
                    elem = a;
                }
                ANYARRAYOID => {
                    n_poly += 1;
                    if a == UNKNOWNOID {
                        have_poly_unknowns = true;
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if array != 0 && a != array {
                        return unsupported("arguments declared \"anyarray\" are not all alike");
                    }
                    array = a;
                }
                ANYRANGEOID => {
                    n_poly += 1;
                    if a == UNKNOWNOID {
                        have_poly_unknowns = true;
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if range != 0 && a != range {
                        return unsupported("arguments declared \"anyrange\" are not all alike");
                    }
                    range = a;
                }
                ANYMULTIRANGEOID => {
                    n_poly += 1;
                    have_anymultirange = true;
                    if a == UNKNOWNOID {
                        have_poly_unknowns = true;
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if multirange != 0 && a != multirange {
                        return unsupported(
                            "arguments declared \"anymultirange\" are not all alike",
                        );
                    }
                    multirange = a;
                }
                ANYCOMPATIBLEOID | ANYCOMPATIBLENONARRAYOID => {
                    have_poly_ac = true;
                    if decl == ANYCOMPATIBLENONARRAYOID {
                        have_ac_nonarray = true;
                    }
                    if a == UNKNOWNOID {
                        continue;
                    }
                    ac_types.push(a);
                }
                ANYCOMPATIBLEARRAYOID => {
                    have_poly_ac = true;
                    have_ac_array = true;
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    let e = self.cat.element_type(a);
                    if e == 0 {
                        return unsupported("argument declared anycompatiblearray is not an array");
                    }
                    ac_types.push(e);
                }
                ANYCOMPATIBLERANGEOID => {
                    have_poly_ac = true;
                    have_ac_range = true;
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if ac_range != 0 {
                        if ac_range != a {
                            return unsupported(
                                "arguments declared \"anycompatiblerange\" are not all alike",
                            );
                        }
                    } else {
                        ac_range = a;
                        ac_range_elem = self.cat.range_subtype(a);
                        if ac_range_elem == 0 {
                            return unsupported(
                                "argument declared anycompatiblerange is not a range type",
                            );
                        }
                        ac_types.push(ac_range_elem);
                    }
                }
                ANYCOMPATIBLEMULTIRANGEOID => {
                    have_poly_ac = true;
                    have_ac_mrange = true;
                    if a == UNKNOWNOID {
                        continue;
                    }
                    a = self.cat.base_type(a);
                    if ac_mrange != 0 {
                        if ac_mrange != a {
                            return unsupported(
                                "arguments declared \"anycompatiblemultirange\" are not all alike",
                            );
                        }
                    } else {
                        ac_mrange = a;
                        ac_mrange_elem = self.cat.multirange_range(a);
                        if ac_mrange_elem == 0 {
                            return unsupported("argument declared anycompatiblemultirange is not a multirange type");
                        }
                    }
                }
                _ => {}
            }
        }
        if n_poly == 0 && !have_poly_ac {
            return Ok(rettype);
        }
        if n_poly > 0 {
            if array != 0 {
                let ae = if array == ANYARRAYOID {
                    if n_poly != 1 || (rettype != ANYARRAYOID && is_polymorphic_family1(rettype)) {
                        return unsupported(
                            "cannot determine element type of \"anyarray\" argument",
                        );
                    }
                    ANYELEMENTOID
                } else {
                    let e = self.cat.element_type(array);
                    if e == 0 {
                        return unsupported("argument declared anyarray is not an array");
                    }
                    e
                };
                if elem == 0 {
                    elem = ae;
                } else if ae != elem {
                    return unsupported("argument declared anyarray is not consistent with argument declared anyelement");
                }
            }
            if multirange != 0 {
                let mr = self.cat.multirange_range(multirange);
                if mr == 0 {
                    return unsupported("argument declared anymultirange is not a multirange type");
                }
                if range == 0 {
                    range = mr;
                } else if mr != range {
                    return unsupported("anymultirange is not consistent with anyrange");
                }
            } else if have_anymultirange && range != 0 {
                multirange = self.cat.range_multirange(range);
            }
            if range != 0 {
                let re = self.cat.range_subtype(range);
                if re == 0 {
                    return unsupported("argument declared anyrange is not a range type");
                }
                if elem == 0 {
                    elem = re;
                } else if re != elem {
                    return unsupported("anyrange is not consistent with anyelement");
                }
            }
            if elem == 0 {
                return unsupported(
                    "could not determine polymorphic type because input has type unknown",
                );
            }
            if have_anynonarray && elem != ANYELEMENTOID && self.cat.type_is_array_domain(elem) {
                return unsupported("type matched to anynonarray is an array type");
            }
            if have_anyenum && elem != ANYELEMENTOID && !self.cat.type_is_enum(elem) {
                return unsupported("type matched to anyenum is not an enum type");
            }
        }
        if have_poly_ac {
            if ac_mrange != 0 {
                if ac_range != 0 {
                    if ac_mrange_elem != ac_range {
                        return unsupported(
                            "anycompatiblemultirange is not consistent with anycompatiblerange",
                        );
                    }
                } else {
                    ac_range = ac_mrange_elem;
                    ac_range_elem = self.cat.range_subtype(ac_range);
                    if ac_range_elem == 0 {
                        return unsupported("not a multirange type");
                    }
                    have_ac_range = true;
                    ac_types.push(ac_range_elem);
                }
            } else if have_ac_mrange && ac_range != 0 {
                ac_mrange = self.cat.range_multirange(ac_range);
            }
            if !ac_types.is_empty() {
                ac_type = self.select_common_type_from_oids(&ac_types, false)?;
                if !ac_types
                    .iter()
                    .all(|a| self.can_coerce_type(&[*a], &[ac_type], CoercionContext::Implicit))
                {
                    return unsupported(
                        "arguments of anycompatible family cannot be cast to a common type",
                    );
                }
                if have_ac_array {
                    ac_array = self.cat.array_type(ac_type);
                    if ac_array == 0 {
                        return unsupported("could not find array type");
                    }
                }
                if have_ac_range {
                    if ac_range == 0 || ac_range_elem != ac_type {
                        return unsupported("anycompatiblerange does not match anycompatible");
                    }
                }
                if have_ac_mrange && (ac_mrange == 0 || ac_range_elem != ac_type) {
                    return unsupported("anycompatiblemultirange does not match anycompatible");
                }
                if have_ac_nonarray && self.cat.type_is_array_domain(ac_type) {
                    return unsupported("type matched to anycompatiblenonarray is an array type");
                }
            } else {
                ac_type = TEXTOID;
                ac_array = TEXTARRAYOID;
                if have_ac_range || have_ac_mrange {
                    return unsupported("could not determine polymorphic type anycompatiblerange");
                }
            }
            for d in declared.iter_mut() {
                match *d {
                    ANYCOMPATIBLEOID | ANYCOMPATIBLENONARRAYOID => *d = ac_type,
                    ANYCOMPATIBLEARRAYOID => *d = ac_array,
                    ANYCOMPATIBLERANGEOID => *d = ac_range,
                    ANYCOMPATIBLEMULTIRANGEOID => *d = ac_mrange,
                    _ => {}
                }
            }
        }
        if have_poly_unknowns {
            for j in 0..n {
                if actual[j] != UNKNOWNOID {
                    continue;
                }
                match declared[j] {
                    ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID => declared[j] = elem,
                    ANYARRAYOID => {
                        if array == 0 {
                            array = self.cat.array_type(elem);
                            if array == 0 {
                                return unsupported("could not find array type");
                            }
                        }
                        declared[j] = array;
                    }
                    ANYRANGEOID => {
                        if range == 0 {
                            return unsupported("could not determine polymorphic type anyrange");
                        }
                        declared[j] = range;
                    }
                    ANYMULTIRANGEOID => {
                        if multirange == 0 {
                            return unsupported(
                                "could not determine polymorphic type anymultirange",
                            );
                        }
                        declared[j] = multirange;
                    }
                    _ => {}
                }
            }
        }
        Ok(match rettype {
            ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID => elem,
            ANYARRAYOID => {
                if array == 0 {
                    array = self.cat.array_type(elem);
                    if array == 0 {
                        return unsupported("could not find array type");
                    }
                }
                array
            }
            ANYRANGEOID => range,
            ANYMULTIRANGEOID => multirange,
            ANYCOMPATIBLEOID | ANYCOMPATIBLENONARRAYOID => ac_type,
            ANYCOMPATIBLEARRAYOID => ac_array,
            ANYCOMPATIBLERANGEOID => ac_range,
            ANYCOMPATIBLEMULTIRANGEOID => ac_mrange,
            r => r,
        })
    }
}

/// `hide_coercion_node`.
fn hide_coercion_node(n: &mut Node) -> R<()> {
    match n {
        Node::FuncExpr(f) => f.funcformat = CoercionForm::ImplicitCast,
        Node::RelabelType(r) => r.relabelformat = CoercionForm::ImplicitCast,
        Node::CoerceViaIO(c) => c.coerceformat = CoercionForm::ImplicitCast,
        Node::ArrayCoerceExpr(c) => c.coerceformat = CoercionForm::ImplicitCast,
        Node::ConvertRowtypeExpr(c) => c.convertformat = CoercionForm::ImplicitCast,
        Node::RowExpr(r) => r.row_format = CoercionForm::ImplicitCast,
        Node::CoerceToDomain(c) => c.coercionformat = CoercionForm::ImplicitCast,
        _ => return unsupported("hide_coercion_node: an unsupported node"),
    }
    Ok(())
}

/// `applyRelabelType`.
pub(crate) fn apply_relabel_type(
    cat: &Catalog,
    arg: Node,
    rtype: Oid,
    rtypmod: i32,
    rcollid: Oid,
    rformat: CoercionForm,
    rlocation: i32,
) -> Node {
    let mut arg = arg;
    while let Node::RelabelType(r) = arg {
        arg = r.arg;
    }
    if let Node::Const(mut c) = arg {
        c.consttype = rtype;
        c.consttypmod = rtypmod;
        c.constcollid = rcollid;
        return Node::Const(c);
    }
    if expr_type(cat, &arg) == rtype
        && expr_typmod(cat, &arg) == rtypmod
        && expr_collation(cat, &arg) == rcollid
    {
        return arg;
    }
    Node::RelabelType(Box::new(RelabelType {
        arg,
        resulttype: rtype,
        resulttypmod: rtypmod,
        resultcollid: rcollid,
        relabelformat: rformat,
        location: rlocation,
    }))
}
