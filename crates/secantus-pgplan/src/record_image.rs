//! The `record_image` operators `*=`, `*<>`, `*<`, `*<=`, `*>` and `*>=`
//! (`record_image_eq` .. `record_image_ge` in PostgreSQL's `rowtypes.c`),
//! which compare two record values by their fields' BINARY images rather
//! than by each type's own operators: `row(1.0) *= row(1.00)` is false,
//! since the two numerics differ in their display scale (RP-650).
//!
//! `record_image_cmp` walks the field pairs in order. A pair of different
//! types is 42804; two NULLs are equal and a NULL sorts after any value; a
//! by-value datum is compared as the unsigned machine word it is stored as
//! (so `-1` sorts after `1`); a varlena is compared by `memcmp` of its data
//! over the shorter length, then by length. The first unequal pair decides,
//! and records of different widths that agree on every common field are
//! 42804. A field type whose image this module does not model is refused
//! by name (0A000) rather than compared some other way.

use std::cmp::Ordering;

use bson::Bson;

use crate::{record_field_types, record_fields, Error, Result};

/// Is `op` one of the `record_image` operators?
pub(crate) fn is_image_operator(op: &str) -> bool {
    matches!(op, "*=" | "*<>" | "*<" | "*<=" | "*>" | "*>=")
}

/// One field's datum, as `record_image_cmp` compares it.
enum Image {
    ByValue(u64),
    Bytes(Vec<u8>),
}

/// `a op b` for two record values; NULL when either is NULL (the
/// functions are strict).
pub(crate) fn compare(op: &str, a: &Bson, b: &Bson) -> Result<Bson> {
    if matches!(a, Bson::Null) || matches!(b, Bson::Null) {
        return Ok(Bson::Null);
    }
    let (Some(fa), Some(fb)) = (record_fields(a), record_fields(b)) else {
        return Err(Error::Unsupported(format!(
            "operator {op} on these operands"
        )));
    };
    let (ta, tb) = (
        record_field_types(a).unwrap_or_default(),
        record_field_types(b).unwrap_or_default(),
    );
    let ty = |types: &[String], i: usize, v: &Bson| {
        types
            .get(i)
            .map(|t| canonical(t))
            .unwrap_or_else(|| crate::inferred_type(v).to_string())
    };
    let mut order = Ordering::Equal;
    for (i, (x, y)) in fa.iter().zip(fb).enumerate() {
        let (t1, t2) = (ty(&ta, i, x), ty(&tb, i, y));
        if t1 != t2 {
            return Err(Error::DatatypeMismatch(format!(
                "cannot compare dissimilar column types {} and {} at record column {}",
                crate::display_type(&t1),
                crate::display_type(&t2),
                i + 1
            )));
        }
        order = match (x, y) {
            (Bson::Null, Bson::Null) => Ordering::Equal,
            (Bson::Null, _) => Ordering::Greater,
            (_, Bson::Null) => Ordering::Less,
            _ => match (image(x, &t1)?, image(y, &t2)?) {
                (Image::ByValue(p), Image::ByValue(q)) => p.cmp(&q),
                (Image::Bytes(p), Image::Bytes(q)) => {
                    let n = p.len().min(q.len());
                    p[..n].cmp(&q[..n]).then(p.len().cmp(&q.len()))
                }
                _ => return Err(Error::Internal("mixed record images".into())),
            },
        };
        if order != Ordering::Equal {
            break;
        }
    }
    if order == Ordering::Equal && fa.len() != fb.len() {
        return Err(Error::DatatypeMismatch(
            "cannot compare record types with different numbers of columns".into(),
        ));
    }
    use Ordering::{Greater, Less};
    Ok(Bson::Boolean(match op {
        "*=" => order == Ordering::Equal,
        "*<>" => order != Ordering::Equal,
        "*<" => order == Less,
        "*<=" => order != Greater,
        "*>" => order == Greater,
        _ => order != Less,
    }))
}

/// A field type's internal name, its typmod dropped.
fn canonical(t: &str) -> String {
    let base = t.split('(').next().unwrap_or(t).trim();
    match base {
        "integer" | "int" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "boolean" => "bool",
        "real" => "float4",
        "double precision" => "float8",
        "decimal" => "numeric",
        "character varying" => "varchar",
        "character" | "char" if !t.starts_with('"') => "bpchar",
        other => other,
    }
    .to_string()
}

/// The datum `record_image_cmp` reads for one non-NULL field of type `ty`.
fn image(v: &Bson, ty: &str) -> Result<Image> {
    let int = |v: &Bson| match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        _ => None,
    };
    let refused = || {
        Error::Unsupported(format!(
            "a record_image comparison over a field of type {ty}"
        ))
    };
    Ok(match ty {
        // A by-value integer is sign-extended into the Datum word
        // (`Int16GetDatum`, `Int32GetDatum`), which is compared unsigned.
        "int2" | "int4" | "int8" => Image::ByValue(int(v).ok_or_else(refused)? as u64),
        // `ObjectIdGetDatum` zero-extends.
        "oid" => Image::ByValue(int(v).ok_or_else(refused)? as u32 as u64),
        "bool" => match v {
            Bson::Boolean(b) => Image::ByValue(u64::from(*b)),
            _ => return Err(refused()),
        },
        "float8" => match v {
            Bson::Double(d) => Image::ByValue(d.to_bits()),
            _ => return Err(refused()),
        },
        // `Float4GetDatum` stores the 32 bits, zero-extended.
        "float4" => match v {
            Bson::Double(d) => Image::ByValue(u64::from((*d as f32).to_bits())),
            _ => return Err(refused()),
        },
        "text" | "varchar" | "bpchar" => match v {
            Bson::String(s) => Image::Bytes(s.as_bytes().to_vec()),
            _ => return Err(refused()),
        },
        "bytea" => match v {
            Bson::Binary(b) => Image::Bytes(b.bytes.clone()),
            _ => return Err(refused()),
        },
        "numeric" => {
            let text = crate::numeric::numeric_text(v)
                .or_else(|| int(v).map(|i| i.to_string()))
                .ok_or_else(refused)?;
            Image::Bytes(numeric_image(&text).ok_or_else(refused)?)
        }
        // An `unknown` field (a bare literal in `row(1, 'a')`) is a cstring,
        // attlen -2, which `record_image_cmp` has no case for.
        "unknown" => {
            return Err(Error::Sqlstate(
                "XX000",
                "unexpected attlen: -2".to_string(),
            ))
        }
        _ => return Err(refused()),
    })
}

/// A numeric's varlena data as `make_result` lays it out: the short
/// format's one 16-bit header, or the long format's sign-and-scale word and
/// weight, then the base-10000 digits, each a native (little-endian) int16,
/// leading and trailing zero digits stripped.
fn numeric_image(text: &str) -> Option<Vec<u8>> {
    const SHORT: u16 = 0x8000;
    match text {
        "NaN" => return Some(0xC000u16.to_le_bytes().to_vec()),
        "Infinity" => return Some(0xD000u16.to_le_bytes().to_vec()),
        "-Infinity" => return Some(0xF000u16.to_le_bytes().to_vec()),
        _ => {}
    }
    let (neg, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let dscale = frac_part.len();
    let int_part = int_part.trim_start_matches('0');
    let int_groups = int_part.len().div_ceil(4);
    let mut digits_text = format!("{:0>w$}", int_part, w = int_groups * 4);
    digits_text.push_str(frac_part);
    while digits_text.len() % 4 != 0 {
        digits_text.push('0');
    }
    let mut digits: Vec<i16> = digits_text
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).ok()?.parse().ok())
        .collect::<Option<_>>()?;
    let mut weight = i64::try_from(int_groups).ok()? - 1;
    let lead = digits.iter().take_while(|d| **d == 0).count();
    digits.drain(..lead);
    weight -= i64::try_from(lead).ok()?;
    while digits.last() == Some(&0) {
        digits.pop();
    }
    let neg = neg && !digits.is_empty();
    if digits.is_empty() {
        weight = 0;
    }
    let mut out = Vec::with_capacity(4 + 2 * digits.len());
    if dscale <= 0x3F && (-64..=63).contains(&weight) {
        let header = SHORT
            | if neg { 0x2000 } else { 0 }
            | (u16::try_from(dscale).ok()? << 7)
            | if weight < 0 { 0x0040 } else { 0 }
            | (weight as u16 & 0x003F);
        out.extend(header.to_le_bytes());
    } else {
        let sign_dscale = if neg { 0x4000 } else { 0 } | (u16::try_from(dscale).ok()? & 0x3FFF);
        out.extend(sign_dscale.to_le_bytes());
        out.extend(i16::try_from(weight).ok()?.to_le_bytes());
    }
    for d in digits {
        out.extend(d.to_le_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::numeric_image;

    /// The bytes `make_result`'s short format lays out for each value on a
    /// little-endian machine (`numeric.c`'s NUMERIC_SHORT header bits).
    #[test]
    fn numeric_images_follow_make_result() {
        // 1.0: weight 0, dscale 1, one digit 1.
        assert_eq!(numeric_image("1.0"), Some(vec![0x80, 0x80, 1, 0]));
        // 1.00: dscale 2, the same digit.
        assert_eq!(numeric_image("1.00"), Some(vec![0x00, 0x81, 1, 0]));
        // 0: no digits.
        assert_eq!(numeric_image("0"), Some(vec![0x00, 0x80]));
        // -12345.6: weight 1, digits 1 2345 6000, negative, dscale 1.
        assert_eq!(
            numeric_image("-12345.6"),
            Some(vec![0x81, 0xA0, 1, 0, 0x29, 0x09, 0x70, 0x17])
        );
        // 0.0001: weight -1, digit 1.
        assert_eq!(numeric_image("0.0001"), Some(vec![0x7F, 0x82, 1, 0]));
    }
}
