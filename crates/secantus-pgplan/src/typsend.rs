//! The types' send functions called by name: `int4send(1)` is the value's
//! binary wire format as a `bytea` (`\x00000001`), as PostgreSQL 15.19's
//! `typsend` functions answer it.
//!
//! The binary format is the wire layer's, which the server installs here
//! ([`set_encoder`]) so one encoder answers both a binary result column and
//! these calls.

use super::*;
use std::sync::OnceLock;

/// Encode a value of a PostgreSQL type in its binary format; `None` when
/// the type has no binary encoding here.
pub type Encoder = fn(&Bson, &str) -> Option<Vec<u8>>;

static ENCODER: OnceLock<Encoder> = OnceLock::new();

/// Install the wire layer's binary encoder.
pub fn set_encoder(f: Encoder) {
    let _ = ENCODER.set(f);
}

/// `typname -> typsend` from PostgreSQL 15.19's `pg_type`, and the send
/// functions several types share (`range_send`), which take their
/// argument's own type.
fn sends() -> &'static std::collections::HashMap<&'static str, Option<&'static str>> {
    static S: OnceLock<std::collections::HashMap<&'static str, Option<&'static str>>> =
        OnceLock::new();
    S.get_or_init(|| {
        let rows: Vec<(&str, &str)> = include_str!("pg_type_io.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut it = l.split('\t');
                let ty = it.next()?;
                it.next()?;
                Some((ty, it.next()?))
            })
            .filter(|(_, send)| *send != "-")
            .collect();
        let mut map = std::collections::HashMap::new();
        for (ty, send) in &rows {
            let shared = rows.iter().filter(|(_, s)| s == send).count() > 1;
            map.insert(*send, (!shared).then_some(*ty));
        }
        for polymorphic in ["array_send", "anyarray_send", "anycompatiblearray_send"] {
            map.insert(polymorphic, None);
        }
        map
    })
}

/// The result type of `name` over one argument, when it is a send function.
pub(crate) fn result_type(name: &str, nargs: usize) -> Option<&'static str> {
    (nargs == 1 && sends().contains_key(name)).then_some("bytea")
}

/// `name(value)`, the value's argument type being `ty`.
pub(crate) fn call(name: &str, value: &Bson, ty: &str) -> Option<Result<Bson>> {
    let declared = *sends().get(name)?;
    if *value == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    // An untyped literal is read as the function's own argument type.
    let (value, ty) = match declared {
        Some(d) if matches!(ty, "" | "unknown") => match cast_value(value.clone(), d) {
            Ok(v) => (v, d.to_string()),
            Err(e) => return Some(Err(e)),
        },
        Some(d) if ty != d && pgtypes::oid_of_name(ty) != pgtypes::oid_of_name(d) => {
            match cast_value(value.clone(), d) {
                Ok(v) => (v, d.to_string()),
                Err(e) => return Some(Err(e)),
            }
        }
        _ => (value.clone(), ty.to_string()),
    };
    // `jsonpath_send`: a version byte (1), then the path's text.
    if ty == "jsonpath" {
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(value_text(&value).as_bytes());
        return Some(Ok(bytea::to_binary(bytes)));
    }
    if let Some(bytes) = own_format(&value, &ty) {
        return Some(bytes.map(bytea::to_binary));
    }
    let encode = ENCODER.get()?;
    Some(
        encode(&value, &ty)
            .map(|bytes| bytea::to_binary(bytes))
            .ok_or_else(|| Error::Unsupported(format!("{name}({ty})"))),
    )
}

/// The send formats of the types the wire layer has no binary encoding for.
fn own_format(value: &Bson, ty: &str) -> Option<Result<Vec<u8>>> {
    let number = |what: &str| -> Result<u64> {
        value_text(value)
            .trim()
            .parse()
            .map_err(|_| Error::Internal(format!("{what}send")))
    };
    Some(match ty {
        "xid" => number("xid").map(|n| (n as u32).to_be_bytes().to_vec()),
        "xid8" => number("xid8").map(|n| n.to_be_bytes().to_vec()),
        "regconfig" => regobj::from_bson(value)
            .map(|(_, oid)| (oid as u32).to_be_bytes().to_vec())
            .ok_or_else(|| Error::Internal("regconfigsend".into())),
        // `tsvectorsend`: the lexeme count, then each lexeme (sorted), a
        // NUL, its position count and each position as `weight << 14 | pos`.
        "tsvector" => fts::parse_vector(&value_text(value)).map(|v| {
            let mut out = (v.0.len() as u32).to_be_bytes().to_vec();
            for (lex, positions) in &v.0 {
                out.extend_from_slice(lex.as_bytes());
                out.push(0);
                out.extend_from_slice(&(positions.len() as u16).to_be_bytes());
                for (p, w) in positions {
                    out.extend_from_slice(&((u16::from(*w) << 14) | p).to_be_bytes());
                }
            }
            out
        }),
        // `tsquerysend`: the item count, then the items as PostgreSQL
        // stores them -- an operator, its right operand, then its left.
        "tsquery" => fts::parse_query(&value_text(value)).map(|q| {
            fn items(q: &fts::Query, out: &mut Vec<u8>) -> u32 {
                use fts::Query as Q;
                match q {
                    Q::Val {
                        lex,
                        prefix,
                        weight,
                    } => {
                        out.extend_from_slice(&[1, *weight, u8::from(*prefix)]);
                        out.extend_from_slice(lex.as_bytes());
                        out.push(0);
                        1
                    }
                    Q::Stop => 0,
                    Q::Not(a) => {
                        out.extend_from_slice(&[2, 1]);
                        1 + items(a, out)
                    }
                    Q::And(l, r) | Q::Or(l, r) | Q::Phrase(l, r, _) => {
                        let op = match q {
                            Q::And(..) => 2,
                            Q::Or(..) => 3,
                            _ => 4,
                        };
                        out.extend_from_slice(&[2, op]);
                        if let Q::Phrase(_, _, distance) = q {
                            out.extend_from_slice(&distance.to_be_bytes());
                        }
                        1 + items(r, out) + items(l, out)
                    }
                }
            }
            let mut body = Vec::new();
            let n = q.as_ref().map_or(0, |q| items(q, &mut body));
            let mut out = n.to_be_bytes().to_vec();
            out.extend(body);
            out
        }),
        _ => return None,
    })
}
