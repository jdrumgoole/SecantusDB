//! `parse_oper.c`, with `namespace.c`'s operator lookups.

use super::catalog::*;
use super::func::{deconstruct_qualified_name, Candidate};
use super::nodefuncs::expr_type;
use super::nodes::*;
use super::parse::Analyzer;
use super::{unsupported, AnalysisError, R};

impl<'a> Analyzer<'a> {
    /// `OpernameGetOprid`.
    fn opername_get_oprid(&self, names: &[String], left: Oid, right: Oid) -> R<Oid> {
        let (schema, name) = deconstruct_qualified_name(names)?;
        if let Some(s) = schema {
            let Some(nsp) = self.cat.namespace_oid(s) else {
                return Ok(0);
            };
            return Ok(self
                .cat
                .opers_named(name)
                .find(|o| o.nsp == nsp && o.left == left && o.right == right)
                .map_or(0, |o| o.oid));
        }
        for nsp in &self.path {
            if let Some(o) = self
                .cat
                .opers_named(name)
                .find(|o| o.nsp == *nsp && o.left == left && o.right == right)
            {
                return Ok(o.oid);
            }
        }
        Ok(0)
    }

    /// `OpernameGetCandidates`.
    fn opername_get_candidates(&self, names: &[String], kind: char) -> R<Vec<Candidate>> {
        let (schema, name) = deconstruct_qualified_name(names)?;
        let nsp = match schema {
            Some(s) => Some(
                self.cat
                    .namespace_oid(s)
                    .ok_or_else(|| AnalysisError::new(format!("schema \"{s}\" does not exist")))?,
            ),
            None => None,
        };
        let mut result: Vec<Candidate> = Vec::new();
        for o in self.cat.opers_named(name) {
            if kind != '\0' && o.kind != kind {
                continue;
            }
            let pathpos = match nsp {
                Some(n) => {
                    if o.nsp != n {
                        continue;
                    }
                    0
                }
                None => {
                    let Some(pos) = self.path.iter().position(|n| *n == o.nsp) else {
                        continue;
                    };
                    if let Some(prev) = result
                        .iter_mut()
                        .find(|r| r.args[0] == o.left && r.args[1] == o.right)
                    {
                        if pos > prev.pathpos {
                            continue;
                        }
                        prev.pathpos = pos;
                        prev.oid = o.oid;
                        continue;
                    }
                    pos
                }
            };
            result.push(Candidate {
                pathpos,
                oid: o.oid,
                nvargs: 0,
                ndargs: 0,
                argnumbers: None,
                args: vec![o.left, o.right],
            });
        }
        // `OpernameGetCandidates` links the results in reverse.
        result.reverse();
        Ok(result)
    }

    /// `binary_oper_exact`.
    fn binary_oper_exact(&self, names: &[String], mut a1: Oid, mut a2: Oid) -> R<Oid> {
        let mut was_unknown = false;
        if a1 == UNKNOWNOID && a2 != 0 {
            a1 = a2;
            was_unknown = true;
        } else if a2 == UNKNOWNOID && a1 != 0 {
            a2 = a1;
            was_unknown = true;
        }
        let r = self.opername_get_oprid(names, a1, a2)?;
        if r != 0 {
            return Ok(r);
        }
        if was_unknown {
            let base = self.cat.base_type(a1);
            if base != a1 {
                return self.opername_get_oprid(names, base, base);
            }
        }
        Ok(0)
    }

    /// `oper_select_candidate`.
    fn oper_select_candidate(&self, input: &[Oid], cands: Vec<Candidate>) -> R<Oid> {
        let cands = self.func_match_argtypes(input, &cands);
        if cands.is_empty() {
            return unsupported("operator does not exist");
        }
        if cands.len() == 1 {
            return Ok(cands[0].oid);
        }
        match self.func_select_candidate(input, cands) {
            Some(c) => Ok(c.oid),
            None => unsupported("operator is not unique"),
        }
    }

    /// `oper`.
    pub fn oper(&self, names: &[String], mut l: Oid, mut r: Oid) -> R<Oid> {
        let exact = self.binary_oper_exact(names, l, r)?;
        if exact != 0 {
            return Ok(exact);
        }
        let clist = self.opername_get_candidates(names, 'b')?;
        if clist.is_empty() {
            return unsupported(format!("operator does not exist: {}", names.join(".")));
        }
        if r == 0 {
            r = l;
        } else if l == 0 {
            l = r;
        }
        self.oper_select_candidate(&[l, r], clist)
    }

    /// `left_oper`.
    fn left_oper(&self, names: &[String], arg: Oid) -> R<Oid> {
        let exact = self.opername_get_oprid(names, 0, arg)?;
        if exact != 0 {
            return Ok(exact);
        }
        let mut clist = self.opername_get_candidates(names, 'l')?;
        if clist.is_empty() {
            return unsupported(format!("operator does not exist: {}", names.join(".")));
        }
        for c in clist.iter_mut() {
            c.args = vec![c.args[1]];
        }
        self.oper_select_candidate(&[arg], clist)
    }

    /// `compatible_oper_opid`.
    pub fn compatible_oper_opid(&self, names: &[String], a1: Oid, a2: Oid) -> R<Oid> {
        let op = self.oper(names, a1, a2)?;
        let o = self
            .cat
            .oper_row(op)
            .ok_or_else(|| AnalysisError::new("an operator".into()))?;
        if super::coerce::is_binary_coercible(self.cat, a1, o.left)
            && super::coerce::is_binary_coercible(self.cat, a2, o.right)
        {
            Ok(op)
        } else {
            unsupported("operator requires run-time type coercion")
        }
    }

    /// `make_op`.
    pub fn make_op(
        &mut self,
        opname: &[String],
        ltree: Option<Node>,
        rtree: Node,
        location: i32,
    ) -> R<Node> {
        let (opid, ltype, rtype) = match &ltree {
            None => {
                let rt = expr_type(self.cat, &rtree);
                (self.left_oper(opname, rt)?, 0, rt)
            }
            Some(l) => {
                let lt = expr_type(self.cat, l);
                let rt = expr_type(self.cat, &rtree);
                (self.oper(opname, lt, rt)?, lt, rt)
            }
        };
        let o = self
            .cat
            .oper_row(opid)
            .ok_or_else(|| AnalysisError::new("an operator".into()))?
            .clone();
        if o.code == 0 {
            return unsupported("operator is only a shell");
        }
        let (mut args, actual, mut declared) = match ltree {
            None => (vec![rtree], vec![rtype], vec![o.right]),
            Some(l) => (vec![l, rtree], vec![ltype, rtype], vec![o.left, o.right]),
        };
        let rettype =
            self.enforce_generic_type_consistency(&actual, &mut declared, o.result, false)?;
        if declared.contains(&INTERNALOID) || rettype == INTERNALOID {
            return unsupported("functions accepting or returning type \"internal\"");
        }
        self.make_fn_arguments(&mut args, &actual, &declared)?;
        let retset = self.cat.func_retset(o.code);
        if retset {
            self.check_srf_call_placement();
        }
        Ok(Node::OpExpr(Box::new(OpExpr {
            kind: OpKind::Op,
            opno: opid,
            opfuncid: o.code,
            opresulttype: rettype,
            opretset: retset,
            opcollid: 0,
            inputcollid: 0,
            args,
            location,
        })))
    }

    /// `make_scalar_array_op`.
    pub fn make_scalar_array_op(
        &mut self,
        opname: &[String],
        use_or: bool,
        ltree: Node,
        rtree: Node,
        location: i32,
    ) -> R<Node> {
        let ltype = expr_type(self.cat, &ltree);
        let atype = expr_type(self.cat, &rtree);
        let rtype = if atype == UNKNOWNOID {
            UNKNOWNOID
        } else {
            let e = self.cat.base_element_type(atype);
            if e == 0 {
                return unsupported("op ANY/ALL (array) requires array on right side");
            }
            e
        };
        let opid = self.oper(opname, ltype, rtype)?;
        let o = self
            .cat
            .oper_row(opid)
            .ok_or_else(|| AnalysisError::new("an operator".into()))?
            .clone();
        if o.code == 0 {
            return unsupported("operator is only a shell");
        }
        let mut actual = vec![ltype, rtype];
        let mut declared = vec![o.left, o.right];
        let rettype =
            self.enforce_generic_type_consistency(&actual, &mut declared, o.result, false)?;
        if rettype != BOOLOID {
            return unsupported("op ANY/ALL (array) requires operator to yield boolean");
        }
        if self.cat.func_retset(o.code) {
            return unsupported("op ANY/ALL (array) requires operator not to return a set");
        }
        let res_atype = if super::coerce::is_polymorphic(declared[1]) {
            atype
        } else {
            let a = self.cat.array_type(declared[1]);
            if a == 0 {
                return unsupported("could not find array type");
            }
            a
        };
        actual[1] = atype;
        declared[1] = res_atype;
        let mut args = vec![ltree, rtree];
        self.make_fn_arguments(&mut args, &actual, &declared)?;
        Ok(Node::ScalarArrayOpExpr(Box::new(ScalarArrayOpExpr {
            opno: opid,
            opfuncid: o.code,
            hashfuncid: 0,
            negfuncid: 0,
            use_or,
            inputcollid: 0,
            args,
            location,
        })))
    }
}
