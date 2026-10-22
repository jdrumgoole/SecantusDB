//! `pg_column_size`: the bytes PostgreSQL's datum for a value takes
//! (RP-074), from the layouts in `postgres.h`, `utils/array.h`,
//! `htup_details.h`, `numeric.c` and `jsonb.h`:
//!
//! - an expression's value is a datum in memory: a varlena carries a 4-byte
//!   header;
//! - a value read from a table column is as the heap stores it: a varlena
//!   of up to 126 data bytes packs to a 1-byte header;
//! - an array is its 16-byte header, 8 bytes a dimension, a null bitmap when
//!   it holds a NULL, all MAXALIGNed, then each element aligned to its
//!   type's alignment after it;
//! - a record is a heap tuple: a 23-byte header (and null bitmap) MAXALIGNed,
//!   then each field aligned to its type, a short varlena packed unaligned.
//!
//! Measured against PostgreSQL 15.19 (2026-10-22). A value past the heap's
//! TOAST threshold (2 kB) may be compressed there, which is not modelled.

use super::*;

/// `(typlen, typalign)`: -1 for a varlena.
fn layout(ty: &str) -> (i32, usize) {
    let base = ty.split('(').next().unwrap_or(ty).trim();
    match base {
        "bool" | "boolean" | "\"char\"" => (1, 1),
        "int2" | "smallint" => (2, 2),
        "int4" | "integer" | "int" | "oid" | "date" | "float4" | "real" | "xid" | "cid"
        | "regclass" | "regtype" | "regproc" | "regprocedure" | "regoper" | "regoperator"
        | "regnamespace" | "regrole" | "regconfig" | "regdictionary" | "regcollation" => (4, 4),
        "int8"
        | "bigint"
        | "float8"
        | "double precision"
        | "time"
        | "timestamp"
        | "timestamptz"
        | "money"
        | "pg_lsn"
        | "xid8"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "time without time zone" => (8, 8),
        "timetz" | "time with time zone" => (12, 8),
        "interval" => (16, 8),
        "uuid" => (16, 1),
        "point" => (16, 8),
        "name" => (64, 1),
        "macaddr" => (6, 4),
        "macaddr8" => (8, 4),
        "tid" => (6, 2),
        // numeric, text, bytea, json(b), inet, arrays, xml, ...: 'i'.
        _ => (-1, 4),
    }
}

fn align(n: usize, a: usize) -> usize {
    n.div_ceil(a) * a
}

/// A `numeric`'s datum: 4-byte header, then the short form's 2-byte header
/// (scale and weight both small) or the long form's 4, then its base-10000
/// digits with leading and trailing zero groups dropped.
fn numeric_size(text: &str) -> usize {
    let t = text.trim();
    if matches!(t, "NaN" | "Infinity" | "-Infinity") {
        return 6;
    }
    let digits = t.trim_start_matches('-');
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let dscale = frac.len();
    let int = int.trim_start_matches('0');
    // Groups of four, aligned on the decimal point.
    let int_groups = int.len().div_ceil(4);
    let padded_int = format!("{}{int}", "0".repeat(int_groups * 4 - int.len()));
    let frac_groups = frac.len().div_ceil(4);
    let padded_frac = format!("{frac}{}", "0".repeat(frac_groups * 4 - frac.len()));
    let groups: Vec<&str> = padded_int
        .as_bytes()
        .chunks(4)
        .chain(padded_frac.as_bytes().chunks(4))
        .map(|c| std::str::from_utf8(c).unwrap_or("0000"))
        .collect();
    let first = groups.iter().position(|g| *g != "0000");
    let Some(first) = first else {
        return 6; // zero: no digits
    };
    let last = groups.iter().rposition(|g| *g != "0000").unwrap_or(first);
    let ndigits = last - first + 1;
    let weight = int_groups as i64 - 1 - first as i64;
    let short = dscale <= 63 && (-64..=63).contains(&weight);
    4 + if short { 2 } else { 4 } + 2 * ndigits
}

/// A jsonb value's bytes from `offset` (relative to the datum's start,
/// which alignment is counted from).
fn jsonb_value(v: &crate::json::Json, offset: usize) -> usize {
    use crate::json::Json as J;
    match v {
        J::Null | J::Bool(_) => 0,
        J::Str(s) => s.len(),
        // A number is a numeric datum, INTALIGNed.
        J::Number(n) => {
            let text = crate::numeric::canonical_numeric_text(n).unwrap_or_else(|_| n.clone());
            align(offset, 4) - offset + numeric_size(&text)
        }
        J::Array(_) | J::Object(_) => {
            align(offset, 4) - offset + jsonb_container(v, align(offset, 4))
        }
    }
}

/// A container: a 4-byte header, a 4-byte JEntry per child (an object's
/// keys, then its values), then the children's data.
fn jsonb_container(v: &crate::json::Json, offset: usize) -> usize {
    use crate::json::Json as J;
    let children: Vec<J> = match v {
        J::Array(a) => a.clone(),
        J::Object(o) => {
            // One entry per key, the last kept; keys in jsonb order:
            // shorter first, then by bytes.
            let mut pairs: Vec<(String, J)> = Vec::new();
            for (k, val) in o {
                pairs.retain(|(pk, _)| pk != k);
                pairs.push((k.clone(), val.clone()));
            }
            pairs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
            let mut c: Vec<J> = pairs.iter().map(|(k, _)| J::Str(k.clone())).collect();
            c.extend(pairs.into_iter().map(|(_, val)| val));
            c
        }
        _ => vec![v.clone()],
    };
    let mut pos = offset + 4 + 4 * children.len();
    for c in &children {
        pos += jsonb_value(c, pos);
    }
    pos - offset
}

fn jsonb_size(text: &str) -> Option<usize> {
    let v = crate::json::parse(text).ok()?;
    // A scalar root is a one-element raw-scalar array.
    Some(4 + jsonb_container(&v, 4))
}

/// The bytes of a varlena value's data (its header excluded).
fn varlena_data(v: &Bson, ty: &str) -> Option<usize> {
    let base = ty.split('(').next().unwrap_or(ty).trim();
    Some(match base {
        "numeric" | "decimal" => numeric_size(&numeric_text(v)?) - 4,
        "jsonb" => jsonb_size(&value_text(v))? - 4,
        "inet" | "cidr" => 2 + if value_text(v).contains(':') { 16 } else { 4 },
        "bytea" => match v {
            Bson::Binary(b) => b.bytes.len(),
            other => value_text(other).len(),
        },
        "bpchar" | "character" | "char" => {
            let text = value_text(v);
            let width = ty
                .split_once('(')
                .and_then(|(_, n)| n.trim_end_matches(')').trim().parse::<usize>().ok())
                .unwrap_or(0);
            text.len() + width.saturating_sub(text.chars().count())
        }
        _ => value_text(v).len(),
    })
}

/// A value of type `ty` as a datum in memory.
fn in_memory(v: &Bson, ty: &str) -> Option<usize> {
    if let Some(element) = ty.strip_suffix("[]") {
        return array_size(v, element);
    }
    if let Some(fields) = record_fields(v) {
        return Some(record_size(v, fields));
    }
    // `unknown` (and cstring) is a NUL-terminated C string.
    if matches!(ty, "unknown" | "cstring") {
        return Some(value_text(v).len() + 1);
    }
    match layout(ty) {
        (-1, _) => Some(4 + varlena_data(v, ty)?),
        (n, _) => Some(n as usize),
    }
}

/// A value as a heap tuple stores it: a short varlena packed.
fn stored(v: &Bson, ty: &str) -> Option<usize> {
    let size = in_memory(v, ty)?;
    let varlena = ty.ends_with("[]") || record_fields(v).is_some() || layout(ty).0 == -1;
    Some(if varlena && size - 4 < 127 {
        size - 3
    } else {
        size
    })
}

fn array_size(v: &Bson, element: &str) -> Option<usize> {
    fn dims(v: &Bson) -> usize {
        match v {
            Bson::Array(items) => 1 + items.first().map_or(0, dims),
            _ => 0,
        }
    }
    fn leaves<'a>(v: &'a Bson, out: &mut Vec<&'a Bson>) {
        match v {
            Bson::Array(items) => items.iter().for_each(|i| leaves(i, out)),
            other => out.push(other),
        }
    }
    let Bson::Array(items) = v else {
        return None;
    };
    if items.is_empty() {
        return Some(16);
    }
    let ndim = dims(v);
    let mut values = Vec::new();
    leaves(v, &mut values);
    let has_null = values.iter().any(|x| matches!(x, Bson::Null));
    let header = 16
        + 8 * ndim
        + if has_null {
            values.len().div_ceil(8)
        } else {
            0
        };
    let mut n = align(header, 8);
    let (_, elem_align) = layout(element);
    for x in values.iter().filter(|x| !matches!(x, Bson::Null)) {
        n += in_memory(x, element)?;
        n = align(n, elem_align);
    }
    Some(n)
}

fn record_size(v: &Bson, fields: &[Bson]) -> usize {
    let types = record_field_types(v).unwrap_or_default();
    let has_null = fields.iter().any(|f| matches!(f, Bson::Null));
    let header = 23
        + if has_null {
            fields.len().div_ceil(8)
        } else {
            0
        };
    let mut n = align(header, 8);
    for (i, f) in fields.iter().enumerate() {
        if matches!(f, Bson::Null) {
            continue;
        }
        let ty = types
            .get(i)
            .cloned()
            .unwrap_or_else(|| inferred_type(f).to_string());
        let varlena = ty.ends_with("[]") || record_fields(f).is_some() || layout(&ty).0 == -1;
        let size = in_memory(f, &ty).unwrap_or(0);
        if varlena && size - 4 < 127 {
            // Packed to a 1-byte header, needing no alignment.
            n += size - 3;
        } else {
            let a = if varlena { 4 } else { layout(&ty).1 };
            n = align(n, a) + size;
        }
    }
    n
}

/// `pg_column_size(v)` for a value of static type `ty`; `stored` when the
/// argument is a table column read straight from the row.
pub(crate) fn column_size(v: &Bson, ty: &str, from_table: bool) -> Option<i32> {
    if matches!(v, Bson::Null) {
        return None;
    }
    let n = if from_table {
        stored(v, ty)
    } else {
        in_memory(v, ty)
    }?;
    i32::try_from(n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_sizes_match_postgresql() {
        for (t, want) in [
            ("1.5", 10),
            ("12345.678", 12),
            ("0", 6),
            ("100000000000000000000", 8),
            ("0.000001", 8),
            ("NaN", 6),
        ] {
            assert_eq!(numeric_size(t), want, "{t}");
        }
    }

    #[test]
    fn jsonb_sizes_match_postgresql() {
        for (t, want) in [("{\"a\":1}", 28), ("[]", 8), ("1", 20)] {
            assert_eq!(jsonb_size(t), Some(want), "{t}");
        }
    }
}
