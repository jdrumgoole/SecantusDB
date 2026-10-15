//! `tid`: a tuple identifier, `(block,offset)`. Input and output transcribed
//! from PostgreSQL 15's `tid.c` (`tidin` / `tidout` / `tidrecv` / `tidsend`).
//!
//! A tid is stored as one Int64, `block * 65536 + offset`, so that comparing
//! and sorting the stored values is `ItemPointerCompare` (block first, then
//! offset) and `min` / `max` need nothing of their own. It prints as `(b,o)`
//! wherever its type is known, as a `pg_lsn` prints as `%X/%X`.

use crate::{Error, Result};
use bson::Bson;

fn bad(input: &str) -> Error {
    Error::InvalidText(format!("invalid input syntax for type tid: \"{input}\""))
}

/// C's `strtoul(s, &end, 10)`: leading `isspace` skipped, an optional sign,
/// then decimal digits. Returns the value (a negative one wrapped, as C
/// does), whether `errno` was set, and the rest of the input. `errno` is
/// `ERANGE` on overflow and, as the BSD libc of the reference server sets
/// it, `EINVAL` when there are no digits: so `'(,)'` is refused there
/// (glibc sets nothing and reads it as `(0,0)`; measured on macOS).
fn strtoul(s: &[u8]) -> (u64, bool, &[u8]) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let start = i;
    let mut value: u64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(s[i] - b'0')))
        {
            Some(v) => value = v,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        return (0, true, s);
    }
    if overflow {
        return (u64::MAX, true, &s[i..]);
    }
    (
        if negative {
            value.wrapping_neg()
        } else {
            value
        },
        false,
        &s[i..],
    )
}

/// `tidin`. The two coordinates start after the first `(` and after each
/// `,`, scanning stops at a `)`, and each is read by `strtoul`: so leading
/// whitespace and a sign are accepted, trailing whitespace is not, anything
/// after the `)` is ignored (`'(1,1)x'` is `(1,1)`), and a block of `-1` is
/// 4294967295 (`tidin` accepts the value either as unsigned or as a
/// sign-extended int32). Measured on PostgreSQL 15.19.
pub fn parse_tid(input: &str) -> Result<i64> {
    let s = input.as_bytes();
    let mut coord: Vec<usize> = Vec::with_capacity(2);
    for (p, &c) in s.iter().enumerate() {
        if coord.len() == 2 || c == b')' {
            break;
        }
        if c == b',' || (c == b'(' && coord.is_empty()) {
            coord.push(p + 1);
        }
    }
    if coord.len() < 2 {
        return Err(bad(input));
    }
    let (cvt, errno, rest) = strtoul(&s[coord[0]..]);
    if errno || rest.first() != Some(&b',') {
        return Err(bad(input));
    }
    let block = cvt as u32;
    if cvt != u64::from(block) && cvt != (block as i32) as i64 as u64 {
        return Err(bad(input));
    }
    let (cvt, errno, rest) = strtoul(&s[coord[1]..]);
    if errno || rest.first() != Some(&b')') || cvt > u64::from(u16::MAX) {
        return Err(bad(input));
    }
    Ok(pack(block, cvt as u16))
}

/// The stored form of `(block,offset)`.
pub fn pack(block: u32, offset: u16) -> i64 {
    (i64::from(block) << 16) | i64::from(offset)
}

/// The `(block, offset)` a stored tid holds.
pub fn unpack(v: i64) -> (u32, u16) {
    ((v >> 16) as u32, (v & 0xffff) as u16)
}

/// `tidout`: `(%u,%u)`.
pub fn render_tid(v: i64) -> String {
    let (b, o) = unpack(v);
    format!("({b},{o})")
}

/// The stored value of a tid: an Int64 (or Int32), or its text.
pub fn tid_of(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int64(i) if (0..1 << 48).contains(i) => Some(*i),
        Bson::Int32(i) if *i >= 0 => Some(i64::from(*i)),
        Bson::String(s) => parse_tid(s).ok(),
        _ => None,
    }
}

/// A stored tid as `tidout` prints it (a value that is not one, as its own
/// text).
pub fn render_tid_value(v: &Bson) -> String {
    tid_of(v).map_or_else(|| crate::value_text(v), render_tid)
}

/// `tidsend`: the block as a u32 and the offset as a u16, big-endian.
pub fn tid_to_wire(v: &Bson) -> Option<Vec<u8>> {
    let (b, o) = unpack(tid_of(v)?);
    let mut out = b.to_be_bytes().to_vec();
    out.extend(o.to_be_bytes());
    Some(out)
}

/// `tidrecv`: six bytes.
pub fn tid_from_wire(bytes: &[u8]) -> Option<Bson> {
    let block = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?);
    let offset = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?);
    (bytes.len() == 6).then(|| Bson::Int64(pack(block, offset)))
}

/// `cast(x AS tid)`: from its text. No other type casts to a tid.
pub fn cast_tid(value: &Bson) -> Result<Bson> {
    match value {
        Bson::Null => Ok(Bson::Null),
        Bson::String(s) => parse_tid(s).map(Bson::Int64),
        // Already a tid (its stored form), as an element of `array[t]`.
        Bson::Int64(i) if tid_of(value).is_some() => Ok(Bson::Int64(*i)),
        other => Err(Error::CannotCoerce(format!(
            "cannot cast type {} to tid",
            crate::inferred_type(other)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<String> {
        parse_tid(s).ok().map(render_tid)
    }

    #[test]
    fn tidin_matches_postgresql() {
        // Each measured on PostgreSQL 15.19.
        assert_eq!(p("(0,1)").as_deref(), Some("(0,1)"));
        assert_eq!(p("(1, 2)").as_deref(), Some("(1,2)"));
        assert_eq!(p(" (1,1)").as_deref(), Some("(1,1)"));
        assert_eq!(p("(1,1)x").as_deref(), Some("(1,1)"));
        assert_eq!(p("(+1,1)").as_deref(), Some("(1,1)"));
        assert_eq!(p("(-0,1)").as_deref(), Some("(0,1)"));
        assert_eq!(p("(010,1)").as_deref(), Some("(10,1)"));
        assert_eq!(p("(-1,1)").as_deref(), Some("(4294967295,1)"));
        assert_eq!(p("(-2147483648,1)").as_deref(), Some("(2147483648,1)"));
        assert_eq!(
            p("(4294967295,65535)").as_deref(),
            Some("(4294967295,65535)")
        );
        for bad in [
            " ( 1 , 2 ) ",
            "(1,2 )",
            "(1 ,2)",
            "(4294967296,1)",
            "(-4294967295,1)",
            "(-2147483649,1)",
            "(1,65536)",
            "(1,-1)",
            "(1,1",
            "1,1)",
            "()",
            "(,)",
            "(1)",
            "(1,2,3)",
            "(0x10,1)",
            "(1.5,1)",
            "",
            "(99999999999999999999,1)",
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
    }

    #[test]
    fn order_is_block_then_offset() {
        assert!(parse_tid("(1,65535)").unwrap() < parse_tid("(2,0)").unwrap());
        assert_eq!(
            tid_from_wire(&tid_to_wire(&Bson::Int64(pack(7, 9))).unwrap()),
            Some(Bson::Int64(pack(7, 9)))
        );
    }
}
