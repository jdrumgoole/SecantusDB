//! `parse_type.c`: type names, and the built-in types' `typmodin`
//! functions.

use super::catalog::*;
use super::func::deconstruct_qualified_name;
use super::nodes::Oid;
use super::parse::Analyzer;
use super::{unsupported, AnalysisError, R};
use pg_query::protobuf as pb;
use pg_query::protobuf::node::Node as N;

impl<'a> Analyzer<'a> {
    /// `LookupTypeName`'s name resolution (no `%TYPE`).
    pub fn lookup_type_name_oid(&self, names: &[String], array: bool) -> R<Oid> {
        let (schema, name) = deconstruct_qualified_name(names)?;
        let t = match schema {
            Some(s) => {
                let nsp = self
                    .cat
                    .namespace_oid(s)
                    .ok_or_else(|| AnalysisError(format!("schema \"{s}\" does not exist")))?;
                self.cat
                    .types_named(name)
                    .find(|t| t.nsp == nsp)
                    .map(|t| t.oid)
            }
            None => self.path.iter().find_map(|n| {
                self.cat
                    .types_named(name)
                    .find(|t| t.nsp == *n)
                    .map(|t| t.oid)
            }),
        };
        let t = t.ok_or_else(|| AnalysisError(format!("type \"{name}\" does not exist")))?;
        Ok(if array { self.cat.array_type(t) } else { t })
    }

    /// `typenameTypeIdAndMod`.
    pub fn typename_type_id_and_mod(&self, tn: &pb::TypeName) -> R<(Oid, i32)> {
        if tn.pct_type {
            return unsupported("%TYPE in a stored rule");
        }
        let names = super::clause::string_list(&tn.names);
        let t = if names.is_empty() {
            tn.type_oid
        } else {
            self.lookup_type_name_oid(&names, !tn.array_bounds.is_empty())?
        };
        if t == 0 {
            return unsupported("a type without an oid");
        }
        let row = self
            .cat
            .type_row(t)
            .ok_or_else(|| AnalysisError(format!("type {t} does not exist")))?;
        if !row.isdefined {
            return unsupported("a shell type");
        }
        if tn.typmods.is_empty() {
            return Ok((t, tn.typemod));
        }
        let mut args = Vec::new();
        for m in &tn.typmods {
            let s = match m.node.as_ref() {
                Some(N::AConst(c)) => match c.val.as_ref() {
                    Some(pb::a_const::Val::Ival(i)) => i.ival.to_string(),
                    Some(pb::a_const::Val::Fval(f)) => f.fval.clone(),
                    Some(pb::a_const::Val::Sval(s)) => s.sval.clone(),
                    _ => return unsupported("type modifiers must be simple constants"),
                },
                Some(N::ColumnRef(c)) if c.fields.len() == 1 => match c.fields[0].node.as_ref() {
                    Some(N::String(s)) => s.sval.clone(),
                    _ => return unsupported("type modifiers must be simple constants"),
                },
                _ => return unsupported("type modifiers must be simple constants"),
            };
            args.push(s);
        }
        // The element type's typmodin serves an array type.
        let base = if row.len == -1 && row.subscript == F_ARRAY_SUBSCRIPT_HANDLER {
            row.elem
        } else {
            t
        };
        Ok((t, typmodin(base, &args)?))
    }
}

fn int_arg(s: &str) -> R<i32> {
    s.parse::<i32>()
        .map_err(|_| AnalysisError("invalid type modifier".into()))
}

/// The built-in `typmodin` functions.
fn typmodin(t: Oid, args: &[String]) -> R<i32> {
    match t {
        // bpchartypmodin / varchartypmodin: n + VARHDRSZ.
        BPCHAROID | VARCHAROID => {
            let [n] = args else {
                return unsupported("invalid type modifier");
            };
            let n = int_arg(n)?;
            if n < 1 {
                return unsupported("length must be at least 1");
            }
            Ok(n + 4)
        }
        // bittypmodin / varbittypmodin.
        1560 | 1562 => {
            let [n] = args else {
                return unsupported("invalid type modifier");
            };
            int_arg(n)
        }
        // numerictypmodin.
        NUMERICOID => {
            let (p, s) = match args {
                [p] => (int_arg(p)?, 0),
                [p, s] => (int_arg(p)?, int_arg(s)?),
                _ => return unsupported("invalid NUMERIC type modifier"),
            };
            if !(1..=1000).contains(&p) || !(-1000..=1000).contains(&s) {
                return unsupported("NUMERIC precision out of range");
            }
            Ok(((p << 16) | (s & 0x7ff)) + 4)
        }
        // anytime_typmodin / anytimestamp_typmodin: clamped to 6.
        TIMEOID | TIMETZOID | TIMESTAMPOID | TIMESTAMPTZOID => {
            let [n] = args else {
                return unsupported("invalid type modifier");
            };
            let n = int_arg(n)?;
            if n < 0 {
                return unsupported("precision must not be negative");
            }
            Ok(n.min(6))
        }
        INTERVALOID => {
            let vals: Vec<i32> = args.iter().map(|a| int_arg(a)).collect::<R<_>>()?;
            const FULL_RANGE: i32 = 0x7FFF;
            const FULL_PRECISION: i32 = 0xFFFF;
            let typmod = |range: i32, prec: i32| ((range & 0x7FFF) << 16) | (prec & 0xFFFF);
            match vals.as_slice() {
                [range] => Ok(typmod(*range, FULL_PRECISION)),
                [range, prec] => {
                    if *prec < 0 {
                        return unsupported("INTERVAL precision must not be negative");
                    }
                    Ok(typmod(*range, (*prec).min(6)))
                }
                _ => {
                    let _ = FULL_RANGE;
                    unsupported("invalid INTERVAL type modifier")
                }
            }
        }
        _ => unsupported("a type modifier for this type"),
    }
}
