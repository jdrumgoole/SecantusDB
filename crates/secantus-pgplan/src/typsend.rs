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
    let encode = ENCODER.get()?;
    Some(
        encode(&value, &ty)
            .map(|bytes| bytea::to_binary(bytes))
            .ok_or_else(|| Error::Unsupported(format!("{name}({ty})"))),
    )
}
