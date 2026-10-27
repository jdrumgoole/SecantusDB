//! `parse_func.c` (with `namespace.c`'s `FuncnameGetCandidates`) and
//! `parse_agg.c`'s aggregate and window-function calls.

use super::catalog::*;
use super::nodefuncs::{expr_location, expr_type};
use super::nodes::*;
use super::parse::{Analyzer, ExprKind};
use super::{pg_error, unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

/// `FuncCandidateList` entry.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub pathpos: usize,
    /// 0: several candidates expand to the same argument list.
    pub oid: Oid,
    pub nvargs: usize,
    pub ndargs: usize,
    pub argnumbers: Option<Vec<usize>>,
    pub args: Vec<Oid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FuncDetail {
    NotFound,
    Multiple,
    Normal,
    Procedure,
    Aggregate,
    WindowFunc,
    Coercion,
}

pub(crate) struct Detail {
    pub code: FuncDetail,
    pub funcid: Oid,
    pub rettype: Oid,
    pub retset: bool,
    pub nvargs: usize,
    pub vatype: Oid,
    pub true_typeids: Vec<Oid>,
    pub argdefault_types: Vec<Oid>,
    pub argnumbers: Option<Vec<usize>>,
}

impl<'a> Analyzer<'a> {
    /// `FuncnameGetCandidates`.
    pub fn funcname_get_candidates(
        &self,
        names: &[String],
        nargs: usize,
        argnames: &[String],
        expand_variadic: bool,
        expand_defaults: bool,
    ) -> R<Vec<Candidate>> {
        let (schema, funcname) = deconstruct_qualified_name(names)?;
        let nsp = match schema {
            Some(s) => match self.cat.namespace_oid(s) {
                Some(o) => Some(o),
                None => return unsupported(format!("schema \"{s}\" does not exist")),
            },
            None => None,
        };
        let mut result: Vec<Candidate> = Vec::new();
        let mut any_special = false;
        for p in self.cat.procs_named(funcname) {
            let pathpos = match nsp {
                Some(n) => {
                    if p.nsp != n {
                        continue;
                    }
                    0
                }
                None => match self.path.iter().position(|n| *n == p.nsp) {
                    Some(pos) => pos,
                    None => continue,
                },
            };
            let pronargs = p.argtypes.len();
            let mut variadic = false;
            let mut va_elem = 0;
            let use_defaults;
            let mut argnumbers = None;
            if !argnames.is_empty() {
                if p.variadic != 0 && expand_variadic {
                    continue;
                }
                if pronargs > nargs && expand_defaults {
                    if nargs + (p.nargdefaults as usize) < pronargs {
                        continue;
                    }
                    use_defaults = true;
                } else {
                    use_defaults = false;
                }
                if pronargs != nargs && !use_defaults {
                    continue;
                }
                match match_named_call(p, nargs, argnames, pronargs) {
                    Some(m) => argnumbers = Some(m),
                    None => continue,
                }
                any_special = true;
            } else {
                if pronargs <= nargs && expand_variadic {
                    va_elem = p.variadic;
                    variadic = va_elem != 0;
                    any_special |= variadic;
                }
                if pronargs > nargs && expand_defaults {
                    if nargs + (p.nargdefaults as usize) < pronargs {
                        continue;
                    }
                    use_defaults = true;
                    any_special = true;
                } else {
                    use_defaults = false;
                }
                if pronargs != nargs && !variadic && !use_defaults {
                    continue;
                }
            }
            let effective = pronargs.max(nargs);
            let mut args = vec![0; effective];
            match &argnumbers {
                Some(nums) => {
                    for i in 0..pronargs {
                        args[i] = p.argtypes[nums[i]];
                    }
                }
                None => args[..pronargs].copy_from_slice(&p.argtypes),
            }
            let nvargs = if variadic {
                for a in args.iter_mut().take(effective).skip(pronargs - 1) {
                    *a = va_elem;
                }
                effective - pronargs + 1
            } else {
                0
            };
            let new = Candidate {
                pathpos,
                oid: p.oid,
                nvargs,
                ndargs: if use_defaults { pronargs - nargs } else { 0 },
                argnumbers,
                args,
            };
            if !result.is_empty() && (any_special || nsp.is_none()) {
                let cmp_nargs = new.args.len() - new.ndargs;
                let prev = result.iter().position(|r| {
                    cmp_nargs == r.args.len() - r.ndargs
                        && new.args[..cmp_nargs] == r.args[..cmp_nargs]
                });
                if let Some(pi) = prev {
                    let prevc = &result[pi];
                    let preference: i64 = if pathpos != prevc.pathpos {
                        pathpos as i64 - prevc.pathpos as i64
                    } else if variadic && prevc.nvargs == 0 {
                        1
                    } else if !variadic && prevc.nvargs > 0 {
                        -1
                    } else {
                        0
                    };
                    if preference > 0 {
                        continue;
                    } else if preference < 0 {
                        result.remove(pi);
                    } else {
                        result[pi].oid = 0;
                        continue;
                    }
                }
            }
            result.insert(0, new);
        }
        Ok(result)
    }

    /// `func_match_argtypes`.
    pub fn func_match_argtypes(&self, input: &[Oid], cands: &[Candidate]) -> Vec<Candidate> {
        let mut out = Vec::new();
        for c in cands {
            if self.can_coerce_type(input, &c.args[..input.len()], CoercionContext::Implicit) {
                out.insert(0, c.clone());
            }
        }
        out
    }

    /// `func_select_candidate`.
    pub fn func_select_candidate(&self, input: &[Oid], cands: Vec<Candidate>) -> Option<Candidate> {
        let nargs = input.len();
        let mut base = Vec::with_capacity(nargs);
        let mut nunknowns = 0;
        for &t in input {
            if t != UNKNOWNOID {
                base.push(self.cat.base_type(t));
            } else {
                base.push(UNKNOWNOID);
                nunknowns += 1;
            }
        }
        // Most exact matches.
        let mut best = Vec::new();
        let mut nbest = 0;
        for c in &cands {
            let nmatch = (0..nargs)
                .filter(|&i| base[i] != UNKNOWNOID && c.args[i] == base[i])
                .count();
            if nmatch > nbest || best.is_empty() {
                nbest = nmatch;
                best = vec![c.clone()];
            } else if nmatch == nbest {
                best.push(c.clone());
            }
        }
        if best.len() == 1 {
            return best.pop();
        }
        let cands = best;
        // Exact or preferred-type matches.
        let slot_category: Vec<char> = base.iter().map(|&t| self.cat.type_category(t)).collect();
        let mut best = Vec::new();
        let mut nbest = 0;
        for c in &cands {
            let nmatch = (0..nargs)
                .filter(|&i| {
                    base[i] != UNKNOWNOID
                        && (c.args[i] == base[i]
                            || self.cat.is_preferred_type(slot_category[i], c.args[i]))
                })
                .count();
            if nmatch > nbest || best.is_empty() {
                nbest = nmatch;
                best = vec![c.clone()];
            } else if nmatch == nbest {
                best.push(c.clone());
            }
        }
        if best.len() == 1 {
            return best.pop();
        }
        let mut cands = best;
        if nunknowns == 0 {
            return None;
        }
        // Resolve unknown positions by category.
        let mut slot_cat = slot_category.clone();
        let mut slot_pref = vec![false; nargs];
        let mut resolved = false;
        let mut failed = false;
        for i in 0..nargs {
            if base[i] != UNKNOWNOID {
                continue;
            }
            resolved = true;
            slot_cat[i] = '\0';
            slot_pref[i] = false;
            let mut conflict = false;
            for c in &cands {
                let (cat, pref) = self.cat.category_preferred(c.args[i]);
                if slot_cat[i] == '\0' {
                    slot_cat[i] = cat;
                    slot_pref[i] = pref;
                } else if cat == slot_cat[i] {
                    slot_pref[i] |= pref;
                } else if cat == 'S' {
                    slot_cat[i] = cat;
                    slot_pref[i] = pref;
                } else {
                    conflict = true;
                }
            }
            if conflict && slot_cat[i] != 'S' {
                failed = true;
                break;
            }
        }
        if resolved && !failed {
            let kept: Vec<Candidate> = cands
                .iter()
                .filter(|c| {
                    (0..nargs).all(|i| {
                        if base[i] != UNKNOWNOID {
                            return true;
                        }
                        let (cat, pref) = self.cat.category_preferred(c.args[i]);
                        cat == slot_cat[i] && !(slot_pref[i] && !pref)
                    })
                })
                .cloned()
                .collect();
            if !kept.is_empty() {
                cands = kept;
            }
            if cands.len() == 1 {
                return cands.pop();
            }
        }
        // Last gasp: one known type for all the unknowns.
        if nunknowns < nargs {
            let mut known = UNKNOWNOID;
            for &t in &base {
                if t == UNKNOWNOID {
                    continue;
                }
                if known == UNKNOWNOID {
                    known = t;
                } else if known != t {
                    known = UNKNOWNOID;
                    break;
                }
            }
            if known != UNKNOWNOID {
                let all = vec![known; nargs];
                let mut found = None;
                let mut n = 0;
                for c in &cands {
                    if self.can_coerce_type(&all, &c.args[..nargs], CoercionContext::Implicit) {
                        n += 1;
                        if n > 1 {
                            break;
                        }
                        found = Some(c.clone());
                    }
                }
                if n == 1 {
                    return found;
                }
            }
        }
        None
    }

    /// `func_get_detail`.
    pub fn func_get_detail(
        &self,
        funcname: &[String],
        fargs: &[Node],
        argnames: &[String],
        argtypes: &[Oid],
        expand_variadic: bool,
        expand_defaults: bool,
    ) -> R<Detail> {
        let nargs = argtypes.len();
        let raw = self.funcname_get_candidates(
            funcname,
            nargs,
            argnames,
            expand_variadic,
            expand_defaults,
        )?;
        let mut best = raw
            .iter()
            .find(|c| nargs == 0 || c.args[..nargs] == *argtypes)
            .cloned();
        let none = |code| Detail {
            code,
            funcid: 0,
            rettype: 0,
            retset: false,
            nvargs: 0,
            vatype: 0,
            true_typeids: Vec::new(),
            argdefault_types: Vec::new(),
            argnumbers: None,
        };
        if best.is_none() {
            if nargs == 1 && !fargs.is_empty() && argnames.is_empty() {
                let target = self.func_name_as_type(funcname);
                if target != 0 {
                    let source = argtypes[0];
                    let iscoercion = if source == UNKNOWNOID && matches!(fargs[0], Node::Const(_)) {
                        true
                    } else {
                        match self.find_coercion_pathway(target, source, CoercionContext::Explicit)
                        {
                            (super::coerce::CoercionPath::Relabel, _) => true,
                            (super::coerce::CoercionPath::CoerceViaIo, _) => {
                                !((source == RECORDOID || self.cat.is_complex(source))
                                    && self.cat.type_category(target) == 'S')
                            }
                            _ => false,
                        }
                    };
                    if iscoercion {
                        return Ok(Detail {
                            code: FuncDetail::Coercion,
                            funcid: 0,
                            rettype: target,
                            retset: false,
                            nvargs: 0,
                            vatype: 0,
                            true_typeids: argtypes.to_vec(),
                            argdefault_types: Vec::new(),
                            argnumbers: None,
                        });
                    }
                }
            }
            if !raw.is_empty() {
                let current = self.func_match_argtypes(argtypes, &raw);
                if current.len() == 1 {
                    best = current.into_iter().next();
                } else if current.len() > 1 {
                    best = self.func_select_candidate(argtypes, current);
                    if best.is_none() {
                        return Ok(none(FuncDetail::Multiple));
                    }
                }
            }
        }
        let Some(best) = best else {
            return Ok(none(FuncDetail::NotFound));
        };
        if best.oid == 0 {
            return Ok(none(FuncDetail::Multiple));
        }
        if !argnames.is_empty()
            && !expand_variadic
            && nargs > 0
            && best
                .argnumbers
                .as_ref()
                .is_some_and(|a| a[nargs - 1] != nargs - 1)
        {
            return Ok(none(FuncDetail::NotFound));
        }
        let p = self.cat.proc_row(best.oid).ok_or_else(|| {
            AnalysisError::new(format!("cache lookup failed for function {}", best.oid))
        })?;
        let mut argdefault_types = Vec::new();
        if best.ndargs > 0 {
            // The defaults' types: each is coerced to its parameter's type
            // when the function is made, so a non-polymorphic parameter's
            // default has that type.
            let first_default = p.argtypes.len() - p.nargdefaults as usize;
            let positions: Vec<usize> = match &best.argnumbers {
                Some(nums) => nums[best.args.len() - best.ndargs..].to_vec(),
                None => (nargs..p.argtypes.len()).collect(),
            };
            let defaults = p
                .argdefaults
                .as_deref()
                .map(split_top_level_commas)
                .unwrap_or_default();
            for pos in positions {
                let declared = p.argtypes[pos];
                if super::coerce::is_polymorphic(declared) {
                    let text = defaults
                        .get(pos - first_default)
                        .ok_or_else(|| AnalysisError::new("a default argument".into()))?;
                    argdefault_types.push(self.default_expr_type(text)?);
                } else {
                    argdefault_types.push(declared);
                }
            }
        }
        let code = match p.kind {
            'a' => FuncDetail::Aggregate,
            'f' => FuncDetail::Normal,
            'p' => FuncDetail::Procedure,
            'w' => FuncDetail::WindowFunc,
            _ => FuncDetail::Normal,
        };
        Ok(Detail {
            code,
            funcid: best.oid,
            rettype: p.rettype,
            retset: p.retset,
            nvargs: best.nvargs,
            vatype: p.variadic,
            true_typeids: best.args.clone(),
            argdefault_types,
            argnumbers: best.argnumbers.clone(),
        })
    }

    /// The type of a polymorphic parameter's default expression.
    fn default_expr_type(&self, text: &str) -> R<Oid> {
        let parsed = pg_query::parse(&format!("SELECT {text}"))
            .map_err(|e| AnalysisError::new(format!("a default: {e}")))?;
        let Some(N::SelectStmt(s)) = parsed
            .protobuf
            .stmts
            .first()
            .and_then(|r| r.stmt.as_ref())
            .and_then(|n| n.node.as_ref())
        else {
            return unsupported("a default argument");
        };
        let Some(N::ResTarget(r)) = s.target_list.first().and_then(|t| t.node.as_ref()) else {
            return unsupported("a default argument");
        };
        let val = r
            .val
            .as_deref()
            .ok_or_else(|| AnalysisError::new("a default".into()))?;
        let ctx = super::parse::AnalysisContext {
            catalog: self.cat,
            search_path: self.path.clone(),
            allow_system_table_mods: false,
        };
        let mut a = Analyzer::new(&ctx);
        a.levels.push(super::parse::ParseState::new());
        let e = a.transform_expr(val, ExprKind::Other)?;
        Ok(expr_type(self.cat, &e))
    }

    /// `FuncNameAsType`.
    fn func_name_as_type(&self, funcname: &[String]) -> Oid {
        let Ok(t) = self.lookup_type_name_oid(funcname, false) else {
            return 0;
        };
        match self.cat.type_row(t) {
            Some(r) if r.isdefined && r.relid == 0 => t,
            _ => 0,
        }
    }

    /// `ParseFuncOrColumn`. `fn_` is None for column syntax, where no
    /// function is an answer of None.
    pub fn parse_func_or_column(
        &mut self,
        funcname: &[String],
        mut fargs: Vec<Node>,
        fn_: Option<&pb::FuncCall>,
        location: i32,
    ) -> R<Option<Node>> {
        let is_column = fn_.is_none();
        let agg_order: &[pb::Node] = fn_.map_or(&[], |f| &f.agg_order);
        let over = fn_.and_then(|f| f.over.as_deref());
        let agg_within_group = fn_.is_some_and(|f| f.agg_within_group);
        let agg_star = fn_.is_some_and(|f| f.agg_star);
        let agg_distinct = fn_.is_some_and(|f| f.agg_distinct);
        let mut func_variadic = fn_.is_some_and(|f| f.func_variadic);
        let funcformat = match fn_.map(|f| f.funcformat) {
            Some(x) if x == pb::CoercionForm::CoerceSqlSyntax as i32 => CoercionForm::SqlSyntax,
            Some(x) if x == pb::CoercionForm::CoerceExplicitCast as i32 => {
                CoercionForm::ExplicitCast
            }
            _ => CoercionForm::ExplicitCall,
        };
        let agg_filter = match fn_.and_then(|f| f.agg_filter.as_deref()) {
            Some(f) => self.transform_where_clause(Some(f), ExprKind::Filter)?,
            None => None,
        };
        let mut actual: Vec<Oid> = fargs.iter().map(|a| expr_type(self.cat, a)).collect();
        let mut argnames = Vec::new();
        for a in &fargs {
            if let Node::NamedArgExpr(na) = a {
                if argnames.contains(&na.name) {
                    return unsupported(format!(
                        "argument name \"{}\" used more than once",
                        na.name
                    ));
                }
                argnames.push(na.name.clone());
            } else if !argnames.is_empty() {
                return unsupported("positional argument cannot follow named argument");
            }
        }
        let nargs = fargs.len();
        let could_be_projection = nargs == 1
            && agg_order.is_empty()
            && agg_filter.is_none()
            && !agg_star
            && !agg_distinct
            && over.is_none()
            && !func_variadic
            && argnames.is_empty()
            && funcname.len() == 1
            && (actual[0] == RECORDOID || self.cat.is_complex(actual[0]));
        if could_be_projection && is_column {
            if let Some(r) = self.parse_complex_projection(&funcname[0], &fargs[0], location)? {
                return Ok(Some(r));
            }
        }
        let d = self.func_get_detail(funcname, &fargs, &argnames, &actual, !func_variadic, true)?;
        let mut aggkind = '\0';
        match d.code {
            FuncDetail::Procedure => return unsupported("a procedure in an expression"),
            FuncDetail::Normal | FuncDetail::Coercion => {
                if agg_star || agg_distinct || agg_within_group || !agg_order.is_empty() {
                    return unsupported("aggregate decoration on a non-aggregate");
                }
                if agg_filter.is_some() || over.is_some() {
                    return unsupported("FILTER or OVER on a non-aggregate");
                }
            }
            FuncDetail::Aggregate => {
                let ag = self.cat.aggregate(d.funcid).ok_or_else(|| {
                    AnalysisError::new("cache lookup failed for aggregate".into())
                })?;
                aggkind = ag.kind;
                if aggkind == 'o' || aggkind == 'h' {
                    if !agg_within_group {
                        return Err(AnalysisError::new(format!(
                            "WITHIN GROUP is required for ordered-set aggregate {}",
                            funcname.join(".")
                        )));
                    }
                    if over.is_some() {
                        return Err(AnalysisError::new(format!(
                            "OVER is not supported for ordered-set aggregate {}",
                            funcname.join(".")
                        )));
                    }
                    // func_get_detail saw one undivided argument list: check
                    // the split into direct and aggregated arguments.
                    let num_aggregated = agg_order.len();
                    let num_direct = nargs - num_aggregated;
                    let cat_direct = ag.numdirectargs as usize;
                    let ok = if d.vatype == 0 {
                        num_direct == cat_direct
                    } else {
                        let mut pronargs = nargs;
                        if d.nvargs > 1 {
                            pronargs -= d.nvargs - 1;
                        }
                        if cat_direct < pronargs {
                            num_direct == cat_direct
                        } else if aggkind == 'h' {
                            d.nvargs == 2 * num_aggregated
                        } else {
                            d.nvargs > num_aggregated
                        }
                    };
                    if !ok {
                        return Err(AnalysisError::new(format!(
                            "function {} does not exist",
                            funcname.join(".")
                        )));
                    }
                    if aggkind == 'h' {
                        self.unify_hypothetical_args(
                            &mut fargs,
                            num_aggregated,
                            &mut actual,
                            &d.true_typeids,
                        )?;
                    }
                } else if agg_within_group {
                    return Err(AnalysisError::new(format!(
                        "{} is not an ordered-set aggregate, so it cannot have WITHIN GROUP",
                        funcname.join(".")
                    )));
                }
            }
            FuncDetail::WindowFunc => {
                if over.is_none() {
                    return unsupported("window function requires an OVER clause");
                }
            }
            FuncDetail::Multiple => {
                if is_column {
                    return Ok(None);
                }
                return unsupported(format!("function {} is not unique", funcname.join(".")));
            }
            FuncDetail::NotFound => {
                if is_column {
                    return Ok(None);
                }
                if could_be_projection {
                    if let Some(r) =
                        self.parse_complex_projection(&funcname[0], &fargs[0], location)?
                    {
                        return Ok(Some(r));
                    }
                }
                return unsupported(format!("function {} does not exist", funcname.join(".")));
            }
        }
        if d.code == FuncDetail::Coercion {
            let arg = fargs.pop().expect("one argument");
            return Ok(Some(self.coerce_type(
                arg,
                actual[0],
                d.rettype,
                -1,
                CoercionContext::Explicit,
                CoercionForm::ExplicitCall,
                location,
            )?));
        }
        actual.extend(d.argdefault_types.iter().copied());
        let mut declared = d.true_typeids.clone();
        let rettype =
            self.enforce_generic_type_consistency(&actual, &mut declared, d.rettype, false)?;
        if declared.contains(&INTERNALOID) || rettype == INTERNALOID {
            return unsupported("functions accepting or returning type \"internal\"");
        }
        // NamedArgExprs learn their parameter positions.
        if let Some(nums) = &d.argnumbers {
            for (i, a) in fargs.iter_mut().enumerate() {
                if let Node::NamedArgExpr(na) = a {
                    na.argnumber = nums[i] as i32;
                }
            }
        }
        self.make_fn_arguments(&mut fargs, &actual, &declared)?;
        if d.vatype == 0 {
            func_variadic = false;
        }
        if d.nvargs > 0 && d.vatype != ANYOID {
            let non_var = nargs - d.nvargs;
            let vargs: Vec<Node> = fargs.split_off(non_var);
            let elem = expr_type(self.cat, &vargs[0]);
            let arr = self.cat.array_type(elem);
            if arr == 0 {
                return unsupported("could not find array type for a VARIADIC argument");
            }
            let loc = vargs
                .iter()
                .map(expr_location)
                .find(|l| *l >= 0)
                .unwrap_or(-1);
            fargs.push(Node::ArrayExpr(Box::new(ArrayExpr {
                array_typeid: arr,
                array_collid: 0,
                element_typeid: elem,
                elements: vargs,
                multidims: false,
                location: loc,
            })));
            func_variadic = true;
        }
        if d.retset {
            self.check_srf_call_placement();
        }
        let node = match d.code {
            FuncDetail::Normal => Node::FuncExpr(Box::new(FuncExpr {
                funcid: d.funcid,
                funcresulttype: rettype,
                funcretset: d.retset,
                funcvariadic: func_variadic,
                funcformat,
                funccollid: 0,
                inputcollid: 0,
                args: fargs,
                location,
            })),
            FuncDetail::Aggregate if over.is_none() => {
                if !argnames.is_empty() {
                    return unsupported("aggregates cannot use named arguments");
                }
                if fargs.is_empty() && !agg_star && !agg_within_group {
                    return unsupported("(*) must be used to call a parameterless aggregate");
                }
                let mut agg = Aggref {
                    aggfnoid: d.funcid,
                    aggtype: rettype,
                    aggcollid: 0,
                    inputcollid: 0,
                    aggtranstype: 0,
                    aggargtypes: Vec::new(),
                    aggdirectargs: Vec::new(),
                    args: Vec::new(),
                    aggorder: Vec::new(),
                    aggdistinct: Vec::new(),
                    aggfilter: agg_filter,
                    aggstar: agg_star,
                    aggvariadic: func_variadic,
                    aggkind,
                    agglevelsup: 0,
                    aggsplit: 0,
                    aggno: -1,
                    aggtransno: -1,
                    location,
                };
                self.transform_aggregate_call(&mut agg, fargs, agg_order, agg_distinct)?;
                Node::Aggref(Box::new(agg))
            }
            _ => {
                let over = over.ok_or_else(|| AnalysisError::new("OVER".into()))?;
                if agg_distinct {
                    return unsupported("DISTINCT is not implemented for window functions");
                }
                if !agg_order.is_empty() {
                    return unsupported(
                        "aggregate ORDER BY is not implemented for window functions",
                    );
                }
                let winagg = d.code == FuncDetail::Aggregate;
                if !winagg && agg_filter.is_some() {
                    return unsupported(
                        "FILTER is not implemented for non-aggregate window functions",
                    );
                }
                let mut w = WindowFunc {
                    winfnoid: d.funcid,
                    wintype: rettype,
                    wincollid: 0,
                    inputcollid: 0,
                    args: fargs,
                    aggfilter: agg_filter,
                    winref: 0,
                    winstar: agg_star,
                    winagg,
                    location,
                };
                self.transform_window_func_call(&mut w, over)?;
                Node::WindowFunc(Box::new(w))
            }
        };
        Ok(Some(node))
    }

    /// `check_srf_call_placement`'s bookkeeping.
    pub fn check_srf_call_placement(&mut self) {
        let ps = self.ps();
        if matches!(
            ps.expr_kind,
            ExprKind::SelectTarget
                | ExprKind::InsertTarget
                | ExprKind::GroupBy
                | ExprKind::OrderBy
                | ExprKind::DistinctOn
                | ExprKind::WindowPartition
                | ExprKind::WindowOrder
                | ExprKind::Values
                | ExprKind::ValuesSingle
        ) {
            ps.has_target_srfs = true;
        }
    }

    /// `make_fn_arguments`.
    pub fn make_fn_arguments(
        &mut self,
        fargs: &mut [Node],
        actual: &[Oid],
        declared: &[Oid],
    ) -> R<()> {
        for (i, a) in fargs.iter_mut().enumerate() {
            if actual[i] != declared[i] {
                if let Node::NamedArgExpr(na) = a {
                    let inner = na.arg.clone();
                    na.arg = self.coerce_type(
                        inner,
                        actual[i],
                        declared[i],
                        -1,
                        CoercionContext::Implicit,
                        CoercionForm::ImplicitCast,
                        -1,
                    )?;
                } else {
                    let n = a.clone();
                    *a = self.coerce_type(
                        n,
                        actual[i],
                        declared[i],
                        -1,
                        CoercionContext::Implicit,
                        CoercionForm::ImplicitCast,
                        -1,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// `ParseComplexProjection`.
    fn parse_complex_projection(
        &mut self,
        funcname: &str,
        first_arg: &Node,
        location: i32,
    ) -> R<Option<Node>> {
        if let Node::Var(v) = first_arg {
            if v.varattno == 0 {
                let up = v.varlevelsup as usize;
                let idx = self.nsitem_by_rtindex(v.varno, up)?;
                return self.scan_nsitem_for_column(up, idx, funcname, location);
            }
        }
        let tupdesc = match first_arg {
            Node::Var(v) if v.vartype == RECORDOID => self.expand_record_variable(v, 0)?,
            other => match self.get_expr_result_tupdesc(other) {
                Ok(t) => t,
                Err(_) => return Ok(None),
            },
        };
        for (i, a) in tupdesc.iter().enumerate() {
            if a.name == funcname && !a.dropped {
                return Ok(Some(Node::FieldSelect(Box::new(FieldSelect {
                    arg: first_arg.clone(),
                    fieldnum: i as i32 + 1,
                    resulttype: a.typid,
                    resulttypmod: a.typmod,
                    resultcollid: a.collation,
                }))));
            }
        }
        Ok(None)
    }

    /// `transformAggregateCall` (plain aggregates).
    /// `unify_hypothetical_args`: each hypothetical direct argument and its
    /// aggregated argument, declared `"any"`, coerced to their common type.
    fn unify_hypothetical_args(
        &mut self,
        fargs: &mut [Node],
        num_aggregated: usize,
        actual: &mut [Oid],
        declared: &[Oid],
    ) -> R<()> {
        let num_direct = fargs.len() - num_aggregated;
        let num_non_hypothetical = num_direct.checked_sub(num_aggregated).ok_or_else(|| {
            AnalysisError::new("incorrect number of arguments to hypothetical-set aggregate".into())
        })?;
        for hargpos in num_non_hypothetical..num_direct {
            let aargpos = num_direct + (hargpos - num_non_hypothetical);
            if declared.get(hargpos) != declared.get(aargpos) {
                return Err(AnalysisError::new(
                    "hypothetical-set aggregate has inconsistent declared argument types".into(),
                ));
            }
            if declared.get(hargpos) != Some(&ANYOID) {
                continue;
            }
            let pair = [fargs[aargpos].clone(), fargs[hargpos].clone()];
            let common = self.select_common_type(&pair, Some("WITHIN GROUP"))?;
            let typmod = self.select_common_typmod(&pair, common);
            let h = fargs[hargpos].clone();
            fargs[hargpos] = self.coerce_type(
                h,
                actual[hargpos],
                common,
                typmod,
                CoercionContext::Implicit,
                CoercionForm::ImplicitCast,
                -1,
            )?;
            actual[hargpos] = common;
            let a = fargs[aargpos].clone();
            fargs[aargpos] = self.coerce_type(
                a,
                actual[aargpos],
                common,
                typmod,
                CoercionContext::Implicit,
                CoercionForm::ImplicitCast,
                -1,
            )?;
            actual[aargpos] = common;
        }
        Ok(())
    }

    fn transform_aggregate_call(
        &mut self,
        agg: &mut Aggref,
        args: Vec<Node>,
        aggorder: &[pb::Node],
        agg_distinct: bool,
    ) -> R<()> {
        if agg.aggkind == 'o' || agg.aggkind == 'h' {
            return self.transform_ordered_set_aggregate_call(agg, args, aggorder);
        }
        let mut tlist = Vec::new();
        let mut attno = 1;
        for a in args {
            tlist.push(TargetEntry {
                expr: a,
                resno: attno,
                resname: None,
                ressortgroupref: 0,
                resorigtbl: 0,
                resorigcol: 0,
                resjunk: false,
            });
            attno += 1;
        }
        let save = self.ps().next_resno;
        self.ps().next_resno = attno;
        let torder = self.transform_sort_clause(aggorder, &mut tlist, ExprKind::OrderBy, true);
        let torder = match torder {
            Ok(t) => t,
            Err(e) => {
                self.ps().next_resno = save;
                return Err(e);
            }
        };
        let tdistinct = if agg_distinct {
            let d = self.transform_distinct_clause(&mut tlist, &torder, true)?;
            if d.iter().any(|s| s.sortop == 0) {
                return unsupported("could not identify an ordering operator for DISTINCT");
            }
            d
        } else {
            Vec::new()
        };
        self.ps().next_resno = save;
        agg.aggargtypes = tlist
            .iter()
            .filter(|t| !t.resjunk)
            .map(|t| expr_type(self.cat, &t.expr))
            .collect();
        agg.args = tlist;
        agg.aggorder = torder;
        agg.aggdistinct = tdistinct;
        self.check_agglevels_and_constraints(agg)
    }

    /// `transformAggregateCall` for an ordered-set aggregate: the
    /// arguments after the direct ones are the aggregated ones, each sorted
    /// as its WITHIN GROUP item says (the SortBy expressions themselves were
    /// transformed as arguments).
    fn transform_ordered_set_aggregate_call(
        &mut self,
        agg: &mut Aggref,
        mut args: Vec<Node>,
        aggorder: &[pb::Node],
    ) -> R<()> {
        let num_direct = args.len() - aggorder.len();
        let aargs = args.split_off(num_direct);
        agg.aggdirectargs = args;
        let mut tlist: Vec<TargetEntry> = Vec::new();
        for (i, a) in aargs.into_iter().enumerate() {
            tlist.push(TargetEntry {
                expr: a,
                resno: i as i32 + 1,
                resname: None,
                ressortgroupref: 0,
                resorigtbl: 0,
                resorigcol: 0,
                resjunk: false,
            });
        }
        let mut torder = Vec::new();
        for (i, o) in aggorder.iter().enumerate() {
            let Some(N::SortBy(sb)) = o.node.as_ref() else {
                return unsupported("WITHIN GROUP's ordering");
            };
            self.add_target_to_sort_list(i, &mut torder, &mut tlist, sb)?;
        }
        let mut argtypes: Vec<Oid> = agg
            .aggdirectargs
            .iter()
            .map(|a| expr_type(self.cat, a))
            .collect();
        argtypes.extend(
            tlist
                .iter()
                .filter(|t| !t.resjunk)
                .map(|t| expr_type(self.cat, &t.expr)),
        );
        agg.aggargtypes = argtypes;
        agg.args = tlist;
        agg.aggorder = torder;
        agg.aggdistinct = Vec::new();
        self.check_agglevels_and_constraints(agg)
    }

    /// `check_agglevels_and_constraints`.
    fn check_agglevels_and_constraints(&mut self, agg: &mut Aggref) -> R<()> {
        let mut min_var: Option<u32> = None;
        let mut min_agg: Option<u32> = None;
        for t in &agg.args {
            super::nodefuncs::agg_levels(&t.expr, 0, &mut min_var, &mut min_agg);
        }
        if let Some(f) = &agg.aggfilter {
            super::nodefuncs::agg_levels(f, 0, &mut min_var, &mut min_agg);
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
        agg.agglevelsup = level;
        if level as usize >= self.depth() {
            return unsupported("an aggregate's level");
        }
        // The direct arguments may hold a Var of the aggregate's level but
        // nothing below it, and no aggregate of it.
        if !agg.aggdirectargs.is_empty() {
            let (mut dvar, mut dagg) = (None, None);
            for a in &agg.aggdirectargs {
                super::nodefuncs::agg_levels(a, 0, &mut dvar, &mut dagg);
            }
            if dvar.is_some_and(|v| v < level) {
                return Err(AnalysisError::new(
                    "outer-level aggregate cannot contain a lower-level variable in its direct arguments"
                        .into(),
                ));
            }
            if dagg.is_some_and(|a| a <= level) {
                return pg_error("42803", "aggregate function calls cannot be nested");
            }
        }
        let kind = self.level_ref(level as usize).expr_kind;
        match kind {
            ExprKind::JoinOn
            | ExprKind::JoinUsing
            | ExprKind::FromSubselect
            | ExprKind::FromFunction
            | ExprKind::Where
            | ExprKind::GroupBy
            | ExprKind::Limit
            | ExprKind::Offset => {
                return unsupported("aggregate functions are not allowed here");
            }
            _ => {}
        }
        self.level(level as usize).has_aggs = true;
        Ok(())
    }

    /// `transformWindowFuncCall`.
    fn transform_window_func_call(&mut self, w: &mut WindowFunc, over: &pb::WindowDef) -> R<()> {
        if !over.name.is_empty() {
            let pos = self
                .psr()
                .windowdefs
                .iter()
                .position(|d| d.name == over.name)
                .ok_or_else(|| {
                    AnalysisError::new(format!("window \"{}\" does not exist", over.name))
                })?;
            w.winref = pos as u32 + 1;
        } else {
            let key = window_def_key(over);
            let found = self.psr().windowdefs.iter().position(|d| {
                let refmatch = (!d.refname.is_empty()
                    && !over.refname.is_empty()
                    && d.refname == over.refname)
                    || (d.refname.is_empty() && over.refname.is_empty());
                refmatch && window_def_key(d) == key
            });
            match found {
                Some(p) => w.winref = p as u32 + 1,
                None => {
                    self.ps().windowdefs.push(over.clone());
                    w.winref = self.psr().windowdefs.len() as u32;
                }
            }
        }
        self.ps().has_window_funcs = true;
        Ok(())
    }
}

/// A window definition's comparable parts (`equal()` ignores locations).
fn window_def_key(d: &pb::WindowDef) -> String {
    let mut d = d.clone();
    d.name.clear();
    d.refname.clear();
    d.location = 0;
    strip_locations(&format!(
        "{:?}",
        (
            &d.partition_clause,
            &d.order_clause,
            d.frame_options,
            &d.start_offset,
            &d.end_offset
        )
    ))
}

fn strip_locations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("location: ") {
        out.push_str(&rest[..i + 10]);
        rest = &rest[i + 10..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '-'))
            .unwrap_or(rest.len());
        out.push('0');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `MatchNamedCall`: the call-order → declaration-order mapping.
fn match_named_call(
    p: &ProcRow,
    nargs: usize,
    argnames: &[String],
    pronargs: usize,
) -> Option<Vec<usize>> {
    let names = p.argnames.as_ref()?;
    let numposargs = nargs - argnames.len();
    let mut argnumbers = vec![0; pronargs];
    let total = p.allargtypes.as_ref().map_or(p.argtypes.len(), Vec::len);
    let mut given = vec![false; total.max(pronargs)];
    for ap in 0..numposargs {
        argnumbers[ap] = ap;
        given[ap] = true;
    }
    let mut ap = numposargs;
    for name in argnames {
        let mut pp = 0;
        let mut found = false;
        for i in 0..total {
            if let Some(modes) = &p.argmodes {
                if !matches!(modes.get(i), Some('i' | 'b' | 'v')) {
                    continue;
                }
            }
            if names.get(i).is_some_and(|n| n == name) {
                if given[pp] {
                    return None;
                }
                given[pp] = true;
                argnumbers[ap] = pp;
                found = true;
                break;
            }
            pp += 1;
        }
        if !found {
            return None;
        }
        ap += 1;
    }
    if nargs < pronargs {
        let first_default = pronargs - p.nargdefaults as usize;
        for pp in numposargs..pronargs {
            if given[pp] {
                continue;
            }
            if pp < first_default {
                return None;
            }
            argnumbers[ap] = pp;
            ap += 1;
        }
    }
    Some(argnumbers)
}

/// `DeconstructQualifiedName`.
pub(crate) fn deconstruct_qualified_name(names: &[String]) -> R<(Option<&str>, &str)> {
    match names {
        [n] => Ok((None, n.as_str())),
        [s, n] => Ok((Some(s.as_str()), n.as_str())),
        [_, s, n] => Ok((Some(s.as_str()), n.as_str())),
        _ => unsupported("improper qualified name (too many dotted names)"),
    }
}

/// Split a deparsed default list on its top-level commas.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut quote = false;
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if quote {
            cur.push(c);
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    cur.push('\'');
                    i += 1;
                } else {
                    quote = false;
                }
            }
        } else {
            match c {
                '\'' => {
                    quote = true;
                    cur.push(c);
                }
                '(' | '[' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur).trim().to_string()),
                _ => cur.push(c),
            }
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}
