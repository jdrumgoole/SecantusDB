//! The checks PostgreSQL's binary receive functions make on a Bind parameter
//! before its value is used (RP-078): how many bytes each type reads, and
//! what it refuses in the header fields it reads. The decoder that builds the
//! value is more forgiving, so a malformed parameter is held to these first,
//! at Bind, with PostgreSQL's SQLSTATEs.
//!
//! Measured on PostgreSQL 15.19 (2026-10-22): a value shorter than the type
//! reads is 08P01 (`no data left in message` for a one-byte read,
//! `insufficient data left in message` otherwise); bytes left over are 22P03
//! `incorrect binary data format in bind parameter N`; a text value must be
//! valid UTF-8 (22021). The per-type header checks follow `inet_recv`,
//! `numeric_recv`, `varbit_recv`, `jsonb_recv` and `array_recv`, in their
//! order.

use pgwire::error::{ErrorInfo, PgWireError};
use postgres_types::{Kind, Type};

/// One `ErrorResponse`.
fn error(code: &str, message: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message.into(),
    )))
}

fn no_data() -> PgWireError {
    error("08P01", "no data left in message")
}

fn insufficient() -> PgWireError {
    error("08P01", "insufficient data left in message")
}

/// A `StringInfo` read cursor, as the receive functions use one.
struct Buf<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Buf<'a> {
    fn left(&self) -> usize {
        self.data.len() - self.pos
    }

    /// `pq_getmsgbyte`.
    fn byte(&mut self) -> Result<u8, PgWireError> {
        let b = *self.data.get(self.pos).ok_or_else(no_data)?;
        self.pos += 1;
        Ok(b)
    }

    /// `pq_getmsgbytes` / `pq_copymsgbytes`.
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], PgWireError> {
        if n > self.left() {
            return Err(insufficient());
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// `pq_getmsgint(buf, 4)`.
    fn i32(&mut self) -> Result<i32, PgWireError> {
        let b = self.bytes(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `pq_getmsgint(buf, 2)`.
    fn u16(&mut self) -> Result<u16, PgWireError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
}

/// The bytes a fixed-width type's receive function reads, and whether it
/// reads them as one `pq_getmsgbyte` (the one-byte types).
fn fixed_width(ty: &Type) -> Option<usize> {
    Some(match *ty {
        Type::BOOL | Type::CHAR => 1,
        Type::INT2 => 2,
        Type::INT4
        | Type::FLOAT4
        | Type::OID
        | Type::DATE
        | Type::XID
        | Type::CID
        | Type::REGPROC
        | Type::REGPROCEDURE
        | Type::REGOPER
        | Type::REGOPERATOR
        | Type::REGCLASS
        | Type::REGTYPE
        | Type::REGNAMESPACE
        | Type::REGROLE
        | Type::REGCONFIG
        | Type::REGDICTIONARY
        | Type::REGCOLLATION => 4,
        Type::INT8
        | Type::FLOAT8
        | Type::TIME
        | Type::TIMESTAMP
        | Type::TIMESTAMPTZ
        | Type::MONEY
        | Type::PG_LSN
        | Type::XID8
        | Type::MACADDR8 => 8,
        Type::MACADDR | Type::TID => 6,
        Type::TIMETZ => 12,
        Type::INTERVAL | Type::UUID | Type::POINT => 16,
        Type::LINE | Type::CIRCLE => 24,
        Type::LSEG | Type::BOX => 32,
        _ => return None,
    })
}

/// A type whose binary form is its text in the server encoding.
fn is_text(ty: &Type) -> bool {
    matches!(
        *ty,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::JSON | Type::UNKNOWN
    )
}

/// `report_invalid_encoding`'s 22021 for the first byte sequence that is not
/// UTF-8 (or a NUL, which no text may hold).
fn check_utf8(bytes: &[u8]) -> Result<(), PgWireError> {
    let bad = match std::str::from_utf8(bytes) {
        Ok(s) => s.find('\0'),
        Err(e) => Some(e.valid_up_to()),
    };
    let Some(at) = bad else {
        return Ok(());
    };
    // The bytes the lead byte claims (`pg_utf_mblen`), as many as there are.
    let lead = bytes[at];
    let claimed = match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    let shown: Vec<String> = bytes[at..(at + claimed).min(bytes.len())]
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect();
    Err(error(
        "22021",
        format!(
            "invalid byte sequence for encoding \"UTF8\": {}",
            shown.join(" ")
        ),
    ))
}

/// Read one value of `ty` from `buf` as its receive function would, leaving
/// the cursor after what it read. `Ok(false)` for a type this module does not
/// model (the decoder judges it).
fn recv(ty: &Type, buf: &mut Buf<'_>, utf8: bool) -> Result<bool, PgWireError> {
    if let Some(width) = fixed_width(ty) {
        if width == 1 {
            buf.byte()?;
        } else {
            buf.bytes(width)?;
        }
        return Ok(true);
    }
    if is_text(ty) {
        let rest = buf.bytes(buf.left())?;
        if utf8 {
            check_utf8(rest)?;
        }
        return Ok(true);
    }
    match *ty {
        Type::BYTEA => {
            buf.bytes(buf.left())?;
            Ok(true)
        }
        Type::JSONB => {
            let version = buf.bytes(1).map(|b| b[0])?;
            if version != 1 {
                return Err(error(
                    "XX000",
                    format!("unsupported jsonb version number {version}"),
                ));
            }
            let rest = buf.bytes(buf.left())?;
            if utf8 {
                check_utf8(rest)?;
            }
            Ok(true)
        }
        Type::INET | Type::CIDR => {
            let what = if *ty == Type::CIDR { "cidr" } else { "inet" };
            let family = buf.byte()?;
            let max_bits = match family {
                2 => 32,
                3 => 128,
                _ => {
                    return Err(error(
                        "22P03",
                        format!("invalid address family in external \"{what}\" value"),
                    ))
                }
            };
            let bits = buf.byte()?;
            if u32::from(bits) > max_bits {
                return Err(error(
                    "22P03",
                    format!("invalid bits in external \"{what}\" value"),
                ));
            }
            buf.byte()?; // is_cidr
            let nb = buf.byte()?;
            if u32::from(nb) != max_bits / 8 {
                return Err(error(
                    "22P03",
                    format!("invalid length in external \"{what}\" value"),
                ));
            }
            for _ in 0..nb {
                buf.byte()?;
            }
            Ok(true)
        }
        Type::BIT | Type::VARBIT => {
            let bitlen = buf.i32()?;
            if bitlen < 0 {
                return Err(error("22P03", "invalid length in external bit string"));
            }
            buf.bytes((bitlen as usize).div_ceil(8))?;
            Ok(true)
        }
        Type::NUMERIC => {
            let ndigits = buf.u16()? as i16;
            if !(0..=(1000 + 1000)).contains(&i32::from(ndigits)) {
                return Err(error(
                    "22P03",
                    "invalid length in external \"numeric\" value",
                ));
            }
            buf.u16()?; // weight
            let sign = buf.u16()?;
            if !matches!(sign, 0x0000 | 0x4000 | 0xC000 | 0xD000 | 0xF000) {
                return Err(error("22P03", "invalid sign in external \"numeric\" value"));
            }
            let dscale = buf.u16()?;
            if dscale & 0x3FFF != dscale {
                return Err(error(
                    "22P03",
                    "invalid scale in external \"numeric\" value",
                ));
            }
            for _ in 0..ndigits {
                if buf.u16()? >= 10000 {
                    return Err(error(
                        "22P03",
                        "invalid digit in external \"numeric\" value",
                    ));
                }
            }
            Ok(true)
        }
        _ => match ty.kind() {
            Kind::Array(element) => recv_array(element, buf, utf8),
            _ => Ok(false),
        },
    }
}

/// `array_recv`: the header, the dimensions, then each element through its
/// own receive function over exactly its length.
fn recv_array(element: &Type, buf: &mut Buf<'_>, utf8: bool) -> Result<bool, PgWireError> {
    let ndim = buf.i32()?;
    if ndim > 6 {
        return Err(error(
            "54000",
            format!("number of array dimensions ({ndim}) exceeds the maximum allowed (6)"),
        ));
    }
    if ndim < 0 {
        return Err(error(
            "22P03",
            format!("invalid number of dimensions: {ndim}"),
        ));
    }
    let flags = buf.i32()?;
    if flags != 0 && flags != 1 {
        return Err(error("22P03", "invalid array flags"));
    }
    let sent = buf.i32()? as u32;
    // Only an element type this server knows is judged: PostgreSQL 15.19
    // refused 1178 and 23 under an `interval[]` and took oids that name no
    // type at all.
    if sent != element.oid() {
        if let Some(sent_ty) = Type::from_oid(sent) {
            return Err(error(
                "42804",
                format!(
                    "binary data has array element type {sent} ({}) instead of expected {} ({})",
                    type_display(&sent_ty),
                    element.oid(),
                    type_display(element),
                ),
            ));
        }
    }
    let mut items: i64 = if ndim == 0 { 0 } else { 1 };
    for _ in 0..ndim {
        let dim = buf.i32()?;
        buf.i32()?; // lower bound
        items = items.saturating_mul(i64::from(dim.max(0)));
    }
    for i in 0..items {
        let len = buf.i32()?;
        if len == -1 {
            continue;
        }
        if len < -1 || len as usize > buf.left() {
            return Err(error("22P03", "insufficient data left in message"));
        }
        let mut item = Buf {
            data: buf.bytes(len as usize)?,
            pos: 0,
        };
        if recv(element, &mut item, utf8)? && item.left() > 0 {
            return Err(error(
                "22P03",
                format!("improper binary format in array element {}", i + 1),
            ));
        }
    }
    Ok(true)
}

/// `format_type`'s name for a built-in type.
fn type_display(ty: &Type) -> String {
    secantus_pgplan::pgtypes::name_of_oid(i64::from(ty.oid()))
        .map(|n| secantus_pgplan::scalar::format_type_text_public(n, None))
        .unwrap_or_else(|| ty.name().to_string())
}

/// Hold binary parameter `index` (0-based), of type `ty`, to its receive
/// function's checks. `utf8` is whether text arrives as UTF-8 (the client
/// encoding), which is all this checks a text value for.
pub(crate) fn check(ty: &Type, bytes: &[u8], index: usize, utf8: bool) -> Result<(), PgWireError> {
    let mut buf = Buf {
        data: bytes,
        pos: 0,
    };
    if recv(ty, &mut buf, utf8)? && buf.left() > 0 {
        return Err(error(
            "22P03",
            format!(
                "incorrect binary data format in bind parameter {}",
                index + 1
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(ty: &Type, hex: &str) -> String {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        match check(ty, &bytes, 0, true) {
            Ok(()) => "ok".into(),
            Err(PgWireError::UserError(e)) => format!("{} {}", e.code, e.message),
            Err(e) => format!("{e:?}"),
        }
    }

    /// Each answer measured on PostgreSQL 15.19 (2026-10-22).
    #[test]
    fn matches_postgresql_receive_functions() {
        let cases: &[(&Type, &str, &str)] = &[
            (&Type::INT4, "00", "08P01 insufficient data left in message"),
            (&Type::INT8, "0000000100", "08P01 insufficient data left in message"),
            (&Type::INT2, "000102", "22P03 incorrect binary data format in bind parameter 1"),
            (&Type::BOOL, "02", "ok"),
            (&Type::CHAR, "", "08P01 no data left in message"),
            (&Type::CHAR, "0102", "22P03 incorrect binary data format in bind parameter 1"),
            (&Type::TEXT, "ff", "22021 invalid byte sequence for encoding \"UTF8\": 0xff"),
            (&Type::TEXT, "00", "22021 invalid byte sequence for encoding \"UTF8\": 0x00"),
            (&Type::VARCHAR, "c3", "22021 invalid byte sequence for encoding \"UTF8\": 0xc3"),
            (&Type::BYTEA, "ff", "ok"),
            (&Type::INET, "", "08P01 no data left in message"),
            (&Type::INET, "0220", "08P01 no data left in message"),
            (&Type::INET, "02200104c0a80001", "ok"),
            (&Type::INET, "0220000501020304", "22P03 invalid length in external \"inet\" value"),
            (&Type::INET, "0221000401020304", "22P03 invalid bits in external \"inet\" value"),
            (&Type::INET, "022000040102030405", "22P03 incorrect binary data format in bind parameter 1"),
            (&Type::JSONB, "", "08P01 insufficient data left in message"),
            (&Type::JSONB, "02", "XX000 unsupported jsonb version number 2"),
            (&Type::VARBIT, "00000008", "08P01 insufficient data left in message"),
            (&Type::VARBIT, "00000008ff", "ok"),
            (&Type::VARBIT, "00000008ffff", "22P03 incorrect binary data format in bind parameter 1"),
            (&Type::NUMERIC, "0001000000000000", "08P01 insufficient data left in message"),
            (&Type::NUMERIC, "00010000000000002710", "22P03 invalid digit in external \"numeric\" value"),
            (&Type::NUMERIC, "0001000050000000", "22P03 invalid sign in external \"numeric\" value"),
            (&Type::NUMERIC, "000100010000FFF00001", "22P03 invalid scale in external \"numeric\" value"),
            (&Type::INT4_ARRAY, "000000010000000000000017", "08P01 insufficient data left in message"),
            (&Type::INT4_ARRAY, "000000010000000000000017000000010000000100000004", "22P03 insufficient data left in message"),
            (&Type::INT4_ARRAY, "00000001000000000000001700000001000000010000000400000005", "ok"),
            (&Type::INT4_ARRAY, "000000010000000000000017000000010000000100000002000000", "08P01 insufficient data left in message"),
            (&Type::INT4_ARRAY, "0000000700000000000000170000000100000001", "54000 number of array dimensions (7) exceeds the maximum allowed (6)"),
            (&Type::INT4_ARRAY, "0000000100000002000000170000000100000001", "22P03 invalid array flags"),
            (&Type::INTERVAL_ARRAY, "0000000100000000010101010000000100000000", "08P01 insufficient data left in message"),
            (&Type::INTERVAL_ARRAY, "000000000000000000000017", "42804 binary data has array element type 23 (integer) instead of expected 1186 (interval)"),
            (&Type::JSON_ARRAY, "000000000000000000000eda", "42804 binary data has array element type 3802 (jsonb) instead of expected 114 (json)"),
        ];
        for (ty, hex, want) in cases {
            assert_eq!(code(ty, hex), *want, "{} {hex}", ty.name());
        }
    }
}
