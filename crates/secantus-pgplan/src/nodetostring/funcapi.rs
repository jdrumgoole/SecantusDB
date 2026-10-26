//! `funcapi.c`: what a function call returns, as a type class and a tuple
//! descriptor.

use super::catalog::*;
use super::nodefuncs::{expr_collation, expr_type, expr_typmod};
use super::nodes::*;
use super::parse::Analyzer;
use super::relation::TupDescAttr;
use super::{unsupported, R};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeFuncClass {
    Scalar,
    Composite,
    CompositeDomain,
    Record,
    Other,
}

impl<'a> Analyzer<'a> {
    /// `get_type_func_class`: (class, base type).
    fn type_func_class(&self, t: Oid) -> (TypeFuncClass, Oid) {
        match self.cat.typtype(t) {
            'c' => (TypeFuncClass::Composite, t),
            'b' | 'e' | 'r' | 'm' => (TypeFuncClass::Scalar, t),
            'd' => {
                let base = self.cat.base_type(t);
                if self.cat.typtype(base) == 'c' {
                    (TypeFuncClass::CompositeDomain, base)
                } else {
                    (TypeFuncClass::Scalar, t)
                }
            }
            'p' => {
                if t == RECORDOID {
                    (TypeFuncClass::Record, t)
                } else if t == VOIDOID || t == CSTRINGOID {
                    (TypeFuncClass::Scalar, t)
                } else {
                    (TypeFuncClass::Other, t)
                }
            }
            _ => (TypeFuncClass::Other, t),
        }
    }

    /// A composite type's descriptor (`lookup_rowtype_tupdesc_copy`).
    fn rowtype_tupdesc(&self, t: Oid) -> Vec<TupDescAttr> {
        self.relation_tupdesc(self.cat.typ_relid(t))
    }

    /// `get_expr_result_type`: (class, result type, descriptor).
    pub fn get_expr_result_type(
        &self,
        e: &Node,
    ) -> R<(TypeFuncClass, Oid, Option<Vec<TupDescAttr>>)> {
        let call = match e {
            Node::FuncExpr(f) => Some((f.funcid, f.args.as_slice(), f.inputcollid)),
            Node::OpExpr(o) => Some((o.opfuncid, o.args.as_slice(), o.inputcollid)),
            _ => None,
        };
        if let Some((funcid, args, inputcoll)) = call {
            return self.internal_get_result_type(funcid, e, args, inputcoll);
        }
        if let Node::RowExpr(r) = e {
            if r.row_typeid == RECORDOID {
                let td = r
                    .args
                    .iter()
                    .zip(r.colnames.iter())
                    .map(|(a, n)| TupDescAttr {
                        name: n.clone(),
                        typid: expr_type(self.cat, a),
                        typmod: expr_typmod(self.cat, a),
                        collation: expr_collation(self.cat, a),
                        dropped: false,
                    })
                    .collect();
                return Ok((TypeFuncClass::Composite, RECORDOID, Some(td)));
            }
        }
        let t = expr_type(self.cat, e);
        let (class, base) = self.type_func_class(t);
        let td = matches!(
            class,
            TypeFuncClass::Composite | TypeFuncClass::CompositeDomain
        )
        .then(|| self.rowtype_tupdesc(base));
        Ok((class, t, td))
    }

    fn internal_get_result_type(
        &self,
        funcid: Oid,
        call: &Node,
        args: &[Node],
        inputcoll: Oid,
    ) -> R<(TypeFuncClass, Oid, Option<Vec<TupDescAttr>>)> {
        let Some(p) = self.cat.proc_row(funcid) else {
            return unsupported(format!("cache lookup failed for function {funcid}"));
        };
        let mut rettype = p.rettype;
        if let Some(mut td) = self.build_function_result_tupdesc(p) {
            if self.resolve_polymorphic_tupdesc(&mut td, &p.argtypes, args, inputcoll) {
                return Ok((TypeFuncClass::Composite, rettype, Some(td)));
            }
            return Ok((TypeFuncClass::Record, rettype, None));
        }
        if super::coerce::is_polymorphic(rettype) {
            rettype = expr_type(self.cat, call);
        }
        let (class, base) = self.type_func_class(rettype);
        let td = matches!(
            class,
            TypeFuncClass::Composite | TypeFuncClass::CompositeDomain
        )
        .then(|| self.rowtype_tupdesc(base));
        Ok((class, rettype, td))
    }

    /// `build_function_result_tupdesc_d`.
    fn build_function_result_tupdesc(&self, p: &ProcRow) -> Option<Vec<TupDescAttr>> {
        let all = p.allargtypes.as_ref()?;
        let modes = p.argmodes.as_ref()?;
        let mut out = Vec::new();
        for (i, t) in all.iter().enumerate() {
            let m = modes.get(i).copied().unwrap_or('i');
            if m == 'i' || m == 'v' {
                continue;
            }
            let name = p
                .argnames
                .as_ref()
                .and_then(|n| n.get(i))
                .filter(|n| !n.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("column{}", out.len() + 1));
            out.push(TupDescAttr {
                name,
                typid: *t,
                typmod: -1,
                collation: self.cat.get_typcollation(*t),
                dropped: false,
            });
        }
        if out.len() < 2 && p.kind != 'p' {
            return None;
        }
        Some(out)
    }

    /// `resolve_polymorphic_tupdesc` (the anyelement / anyarray family).
    fn resolve_polymorphic_tupdesc(
        &self,
        td: &mut [TupDescAttr],
        declared: &[Oid],
        args: &[Node],
        inputcoll: Oid,
    ) -> bool {
        if !td.iter().any(|a| super::coerce::is_polymorphic(a.typid)) {
            return true;
        }
        let mut elem = 0;
        let mut array = 0;
        for (i, d) in declared.iter().enumerate() {
            let actual = args.get(i).map_or(0, |a| expr_type(self.cat, a));
            match *d {
                ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID if elem == 0 => {
                    if actual == 0 {
                        return false;
                    }
                    elem = actual;
                }
                ANYARRAYOID if array == 0 => {
                    if actual == 0 {
                        return false;
                    }
                    array = actual;
                }
                _ => {}
            }
        }
        let need_elem = td
            .iter()
            .any(|a| matches!(a.typid, ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID));
        let need_array = td.iter().any(|a| a.typid == ANYARRAYOID);
        if need_elem && elem == 0 {
            elem = self.cat.element_type(self.cat.base_type(array));
        }
        if need_array && array == 0 {
            array = self.cat.array_type(elem);
        }
        let mut anycoll = if elem != 0 {
            self.cat.get_typcollation(elem)
        } else if array != 0 {
            self.cat.get_typcollation(array)
        } else {
            0
        };
        if anycoll != 0 && inputcoll != 0 {
            anycoll = inputcoll;
        }
        for a in td.iter_mut() {
            match a.typid {
                ANYELEMENTOID | ANYNONARRAYOID | ANYENUMOID => {
                    a.typid = elem;
                    a.typmod = -1;
                    a.collation = anycoll;
                }
                ANYARRAYOID => {
                    a.typid = array;
                    a.typmod = -1;
                    a.collation = anycoll;
                }
                t if super::coerce::is_polymorphic(t) => return false,
                _ => {}
            }
        }
        true
    }

    /// `get_expr_result_tupdesc`.
    pub fn get_expr_result_tupdesc(&self, e: &Node) -> R<Vec<TupDescAttr>> {
        let (class, _, td) = self.get_expr_result_type(e)?;
        match (class, td) {
            (TypeFuncClass::Composite | TypeFuncClass::CompositeDomain, Some(td)) => Ok(td),
            _ => unsupported("record type has not been registered"),
        }
    }
}
