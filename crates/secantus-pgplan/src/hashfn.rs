//! The hash support functions called by name -- `hashint4(1)`,
//! `hashtext('a')`, `hashtextextended('a', 0)` -- answering PostgreSQL
//! 15.19's values: `hash_bytes` and `hash_bytes_uint32` (Bob Jenkins'
//! lookup3, as `src/common/hashfn.c` has it) over the bytes each type's hash
//! function feeds them (`hashfunc.c`, `network.c`, `uuid.c`, `date.c`,
//! `timestamp.c`, `rangetypes.c`, `multirangetypes.c`, `numeric.c`,
//! `arrayfuncs.c`). The `*extended` forms take a 64-bit seed and answer an
//! int8.
//!
//! A value's bytes are what PostgreSQL holds in memory on a little-endian
//! machine (an int16 numeric digit, a float8, a tid's three uint16s).

use super::*;

fn rot(x: u32, k: u32) -> u32 {
    x.rotate_left(k)
}

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= rot(*c, 4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= rot(*a, 6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= rot(*b, 8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= rot(*c, 16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= rot(*a, 19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= rot(*b, 4);
    *b = b.wrapping_add(*a);
}

fn final_mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 14));
    *a ^= *c;
    *a = a.wrapping_sub(rot(*c, 11));
    *b ^= *a;
    *b = b.wrapping_sub(rot(*a, 25));
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 16));
    *a ^= *c;
    *a = a.wrapping_sub(rot(*c, 4));
    *b ^= *a;
    *b = b.wrapping_sub(rot(*a, 14));
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 24));
}

/// `hash_bytes` / `hash_bytes_extended`: `(b, c)` after the final mix.
fn bytes_state(k: &[u8], seed: Option<u64>) -> (u32, u32) {
    let init = 0x9e37_79b9u32
        .wrapping_add(k.len() as u32)
        .wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (init, init, init);
    if let Some(seed) = seed.filter(|s| *s != 0) {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix(&mut a, &mut b, &mut c);
    }
    let word = |s: &[u8]| u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
    let mut rest = k;
    while rest.len() >= 12 {
        a = a.wrapping_add(word(&rest[0..4]));
        b = b.wrapping_add(word(&rest[4..8]));
        c = c.wrapping_add(word(&rest[8..12]));
        mix(&mut a, &mut b, &mut c);
        rest = &rest[12..];
    }
    // The last eleven bytes; the lowest byte of `c` is the length's.
    let byte = |i: usize, shift: u32| u32::from(rest[i]) << shift;
    let len = rest.len();
    if len >= 11 {
        c = c.wrapping_add(byte(10, 24));
    }
    if len >= 10 {
        c = c.wrapping_add(byte(9, 16));
    }
    if len >= 9 {
        c = c.wrapping_add(byte(8, 8));
    }
    if len >= 8 {
        b = b.wrapping_add(byte(7, 24));
    }
    if len >= 7 {
        b = b.wrapping_add(byte(6, 16));
    }
    if len >= 6 {
        b = b.wrapping_add(byte(5, 8));
    }
    if len >= 5 {
        b = b.wrapping_add(byte(4, 0));
    }
    if len >= 4 {
        a = a.wrapping_add(byte(3, 24));
    }
    if len >= 3 {
        a = a.wrapping_add(byte(2, 16));
    }
    if len >= 2 {
        a = a.wrapping_add(byte(1, 8));
    }
    if len >= 1 {
        a = a.wrapping_add(byte(0, 0));
    }
    final_mix(&mut a, &mut b, &mut c);
    (b, c)
}

/// `hash_bytes_uint32` / `hash_bytes_uint32_extended`.
fn uint32_state(k: u32, seed: Option<u64>) -> (u32, u32) {
    let init = 0x9e37_79b9u32.wrapping_add(4).wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (init, init, init);
    if let Some(seed) = seed.filter(|s| *s != 0) {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix(&mut a, &mut b, &mut c);
    }
    a = a.wrapping_add(k);
    final_mix(&mut a, &mut b, &mut c);
    (b, c)
}

/// One hash: 32-bit, or 64-bit under a seed.
#[derive(Clone, Copy)]
enum H {
    Plain,
    Seed(u64),
}

impl H {
    fn bytes(self, k: &[u8]) -> u64 {
        match self {
            H::Plain => u64::from(bytes_state(k, None).1),
            H::Seed(s) => {
                let (b, c) = bytes_state(k, Some(s));
                (u64::from(b) << 32) | u64::from(c)
            }
        }
    }

    fn uint32(self, k: u32) -> u64 {
        match self {
            H::Plain => u64::from(uint32_state(k, None).1),
            H::Seed(s) => {
                let (b, c) = uint32_state(k, Some(s));
                (u64::from(b) << 32) | u64::from(c)
            }
        }
    }

    /// `hashint8`: the halves folded by the sign, then one uint32.
    fn int8(self, v: i64) -> u64 {
        let lo = v as u32;
        let hi = (v >> 32) as u32;
        self.uint32(lo ^ if v >= 0 { hi } else { !hi })
    }

    fn float8(self, v: f64) -> u64 {
        if v == 0.0 {
            return match self {
                H::Plain => 0,
                H::Seed(s) => s,
            };
        }
        let v = if v.is_nan() { f64::NAN } else { v };
        self.bytes(&v.to_le_bytes())
    }
}

fn text_of(v: &Bson, ty: &str) -> Result<String> {
    if ty.is_empty() || ty == "unknown" {
        return Ok(value_text(v));
    }
    // The type's own rendering: a `pg_lsn` or `tid` is carried in a form
    // of its own, which only the text cast spells as written.
    let typed = cast_value(v.clone(), ty)?;
    Ok(match cast_value(typed.clone(), "text") {
        Ok(Bson::String(s)) => s,
        _ => value_text(&typed),
    })
}

fn parse<T: std::str::FromStr>(s: &str, what: &str) -> Result<T> {
    s.trim()
        .parse()
        .map_err(|_| Error::Internal(format!("hash of this {what}")))
}

/// Seconds, minutes and hours of `HH:MM:SS[.ffffff]` as microseconds.
fn clock_micros(s: &str) -> Result<i64> {
    let mut parts = s.split(':');
    let h: i64 = parse(parts.next().unwrap_or("0"), "time")?;
    let m: i64 = parse(parts.next().unwrap_or("0"), "time")?;
    let sec = parts.next().unwrap_or("0");
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let s: i64 = parse(whole, "time")?;
    let frac: i64 = format!("{frac:0<6}")[..6].parse().unwrap_or(0);
    Ok(((h * 60 + m) * 60 + s) * 1_000_000 + frac)
}

/// A `time` as microseconds since midnight.
pub(crate) fn time_micros(v: &Bson) -> Result<i64> {
    clock_micros(&text_of(v, "time")?)
}

/// A `timetz` as PostgreSQL holds it: microseconds since midnight in its
/// own zone, and the zone in seconds WEST of Greenwich.
pub(crate) fn timetz_parts(v: &Bson) -> Result<(i64, i32)> {
    let t = text_of(v, "timetz")?;
    let at = t.rfind(['+', '-']).unwrap_or(t.len());
    let (clock, zone) = t.split_at(at);
    let micros = clock_micros(clock)?;
    let sign = if zone.starts_with('-') { 1 } else { -1 };
    let mut parts = zone[1.min(zone.len())..].split(':');
    let hh: i32 = parse(parts.next().unwrap_or("0"), "timetz")?;
    let mm: i32 = parse(parts.next().unwrap_or("0"), "timetz")?;
    let ss: i32 = parse(parts.next().unwrap_or("0"), "timetz")?;
    Ok((micros, sign * (hh * 3600 + mm * 60 + ss)))
}

/// PostgreSQL's timestamp: microseconds since 2000-01-01 00:00.
fn timestamp_micros(v: &Bson) -> Result<i64> {
    let text = text_of(v, "timestamp")?;
    let micros = parse_timestamp(&text)?;
    Ok(micros - 946_684_800_000_000)
}

/// `inet`'s in-memory bytes: family (2 for IPv4, 3 for IPv6), the prefix
/// length, then the address.
pub(crate) fn inet_bytes(text: &str) -> Result<Vec<u8>> {
    let (addr, bits) = match text.split_once('/') {
        Some((a, b)) => (a, Some(parse::<u8>(b, "inet")?)),
        None => (text, None),
    };
    let ip: std::net::IpAddr = parse(addr, "inet")?;
    Ok(match ip {
        std::net::IpAddr::V4(v4) => {
            let mut out = vec![2, bits.unwrap_or(32)];
            out.extend_from_slice(&v4.octets());
            out
        }
        std::net::IpAddr::V6(v6) => {
            let mut out = vec![3, bits.unwrap_or(128)];
            out.extend_from_slice(&v6.octets());
            out
        }
    })
}

fn hex_bytes(text: &str) -> Result<Vec<u8>> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| Error::Internal("hex".into())))
        .collect()
}

/// A numeric's base-10000 digits and the weight of the first, as
/// `hash_numeric` sees them: leading and trailing zero digits dropped.
fn numeric_digits(text: &str) -> Option<(Vec<i16>, i32)> {
    let t = text.trim().trim_start_matches(['-', '+']);
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    let int = int.trim_start_matches('0');
    // Pad the integer part on the left and the fraction on the right to
    // whole groups of four.
    let lead = (4 - int.len() % 4) % 4;
    let int = format!("{}{int}", "0".repeat(lead));
    let trail = (4 - frac.len() % 4) % 4;
    let frac = format!("{frac}{}", "0".repeat(trail));
    let mut groups: Vec<i16> = Vec::new();
    for chunk in int.as_bytes().chunks(4).chain(frac.as_bytes().chunks(4)) {
        groups.push(std::str::from_utf8(chunk).ok()?.parse().ok()?);
    }
    let mut weight = i32::try_from(int.len() / 4).ok()? - 1;
    let first = groups.iter().position(|d| *d != 0)?;
    weight -= i32::try_from(first).ok()?;
    let last = groups.iter().rposition(|d| *d != 0)?;
    Some((groups[first..=last].to_vec(), weight))
}

fn hash_numeric(v: &Bson, h: H) -> Result<u64> {
    let text = text_of(v, "numeric")?;
    if matches!(text.trim(), "NaN" | "Infinity" | "-Infinity") {
        return Ok(match h {
            H::Plain => 0,
            H::Seed(s) => s,
        });
    }
    let Some((digits, weight)) = numeric_digits(&text) else {
        // Zero.
        return Ok(match h {
            H::Plain => u64::from(u32::MAX),
            H::Seed(s) => s.wrapping_sub(1),
        });
    };
    let bytes: Vec<u8> = digits.iter().flat_map(|d| d.to_le_bytes()).collect();
    let hash = h.bytes(&bytes);
    Ok(match h {
        H::Plain => u64::from((hash as u32) ^ (weight as u32)),
        H::Seed(_) => hash ^ (i64::from(weight) as u64),
    })
}

/// The hash of one value of `ty`.
fn hash_value(v: &Bson, ty: &str, h: H) -> Result<u64> {
    let base = ty.trim_start_matches("pg_catalog.");
    match base {
        "int2" | "int4" => Ok(h.uint32(parse::<i64>(&text_of(v, base)?, "integer")? as i32 as u32)),
        "int8" => Ok(h.int8(parse(&text_of(v, base)?, "bigint")?)),
        "oid" => Ok(h.uint32(parse(&text_of(v, base)?, "oid")?)),
        "\"char\"" | "char" => {
            let t = text_of(v, "\"char\"")?;
            Ok(h.uint32(u32::from(t.bytes().next().unwrap_or(0))))
        }
        "float4" | "float8" => {
            let x: f64 = match v {
                Bson::Double(d) => *d,
                other => parse(&text_of(other, base)?, "float")?,
            };
            Ok(h.float8(x))
        }
        "text" | "varchar" | "name" | "unknown" | "" => Ok(h.bytes(text_of(v, "text")?.as_bytes())),
        "bpchar" => Ok(h.bytes(text_of(v, "bpchar")?.trim_end_matches(' ').as_bytes())),
        "macaddr" | "macaddr8" | "uuid" => Ok(h.bytes(&hex_bytes(&text_of(v, base)?)?)),
        "inet" | "cidr" => Ok(h.bytes(&inet_bytes(&text_of(v, base)?)?)),
        // A tid is carried as one Int64, `block * 65536 + offset`, and
        // hashed as its three uint16s (`bi_hi`, `bi_lo`, `ip_posid`).
        "tid" => {
            let n = match cast_value(v.clone(), "tid")? {
                Bson::Int64(n) => n,
                Bson::Int32(n) => i64::from(n),
                _ => return Err(Error::Internal("hash of this tid".into())),
            };
            let (block, offset) = ((n >> 16) as u32, (n & 0xffff) as u16);
            let mut bytes = Vec::with_capacity(6);
            bytes.extend_from_slice(&((block >> 16) as u16).to_le_bytes());
            bytes.extend_from_slice(&(block as u16).to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            Ok(h.bytes(&bytes))
        }
        // `pg_lsn_hash` is `hashint8` of the position.
        "pg_lsn" => {
            let lsn = systypes::lsn_of(v)
                .or_else(|| systypes::lsn_of(&cast_value(v.clone(), "pg_lsn").ok()?))
                .ok_or_else(|| Error::Internal("hash of this pg_lsn".into()))?;
            Ok(h.int8(lsn as i64))
        }
        "time" => Ok(h.int8(time_micros(v)?)),
        "timestamp" => Ok(h.int8(timestamp_micros(v)?)),
        "timetz" => {
            let (micros, zone_secs) = timetz_parts(v)?;
            Ok(h.int8(micros) ^ h.uint32(zone_secs as u32))
        }
        "interval" => {
            let iv = Interval::from_bson(&cast_value(v.clone(), "interval")?)
                .ok_or_else(|| Error::Internal("hash of this interval".into()))?;
            let days = i128::from(iv.months) * 30 + i128::from(iv.days);
            let span = days * 86_400_000_000 + i128::from(iv.micros);
            Ok(h.int8(span as i64))
        }
        "numeric" => hash_numeric(v, h),
        _ => Err(Error::Unsupported(format!("hashing a value of type {ty}"))),
    }
}

/// `hash_array`: each element's hash folded as `hash * 31 + element`.
fn hash_array(v: &Bson, element: &str, h: H) -> Result<u64> {
    let Bson::Array(items) = arrays::strip(v) else {
        return Err(Error::Internal("hash of this array".into()));
    };
    let mut flat = Vec::new();
    fn flatten(items: &[Bson], out: &mut Vec<Bson>) {
        for i in items {
            match i {
                Bson::Array(inner) => flatten(inner, out),
                other => out.push(other.clone()),
            }
        }
    }
    flatten(&items, &mut flat);
    let mut result: u64 = 1;
    for item in &flat {
        let e = if *item == Bson::Null {
            0
        } else {
            hash_value(item, element, h)?
        };
        result = match h {
            H::Plain => u64::from(
                (result as u32)
                    .wrapping_shl(5)
                    .wrapping_sub(result as u32)
                    .wrapping_add(e as u32),
            ),
            H::Seed(_) => result.wrapping_shl(5).wrapping_sub(result).wrapping_add(e),
        };
    }
    Ok(result)
}

/// One range's merged hash: its flags', then each bound's.
fn range_hash(r: &range::Range, element: &str, h: H) -> Result<u64> {
    const EMPTY: u32 = 0x01;
    const LB_INC: u32 = 0x02;
    const UB_INC: u32 = 0x04;
    const LB_INF: u32 = 0x08;
    const UB_INF: u32 = 0x10;
    let mut flags = 0u32;
    if r.empty {
        flags |= EMPTY;
    } else {
        if r.lower.is_none() {
            flags |= LB_INF;
        } else if r.lower_inc {
            flags |= LB_INC;
        }
        if r.upper.is_none() {
            flags |= UB_INF;
        } else if r.upper_inc {
            flags |= UB_INC;
        }
    }
    let bound = |b: &Option<String>| -> Result<u64> {
        match b {
            Some(v) if !r.empty => hash_value(&Bson::String(v.clone()), element, h),
            _ => Ok(0),
        }
    };
    let (lower, upper) = (bound(&r.lower)?, bound(&r.upper)?);
    Ok(match h {
        H::Plain => {
            let mut x = (h.uint32(flags) as u32) ^ (lower as u32);
            x = x.rotate_left(1);
            u64::from(x ^ upper as u32)
        }
        H::Seed(_) => {
            let x = h.uint32(flags) ^ lower;
            let x = ((x << 1) & 0xffff_fffe_ffff_fffe) | ((x >> 31) & 0x0000_0001_0000_0001);
            x ^ upper
        }
    })
}

/// `jsonb_hash` / `jsonb_hash_extended`: the iterator's tokens folded in --
/// an array's or object's flag at its start, each key, value and element
/// as `JsonbHashScalarValue` hashes it -- with object keys in jsonb's own
/// order (by length, then bytes; the last of a duplicate kept). A raw
/// scalar is a one-element array; an empty container hashes as 0 (the seed).
fn jsonb_hash(text: &str, h: H) -> Result<u64> {
    const ARRAY: u32 = 0x4000_0000;
    const OBJECT: u32 = 0x2000_0000;
    let j = json::parse(text).map_err(|_| Error::Internal("jsonb_hash".into()))?;
    let empty = match &j {
        json::Json::Array(items) => items.is_empty(),
        json::Json::Object(pairs) => pairs.is_empty(),
        _ => false,
    };
    if empty {
        return Ok(match h {
            H::Plain => 0,
            H::Seed(s) => s,
        });
    }
    let flag = |f: u32| match h {
        H::Plain => u64::from(f),
        H::Seed(_) => (u64::from(f) << 32) | u64::from(f),
    };
    let rotate = |x: u64| match h {
        H::Plain => u64::from((x as u32).rotate_left(1)),
        H::Seed(_) => ((x << 1) & 0xffff_fffe_ffff_fffe) | ((x >> 31) & 0x0000_0001_0000_0001),
    };
    let scalar = |v: &json::Json, hash: &mut u64| -> Result<()> {
        let tmp = match v {
            json::Json::Null => match h {
                H::Plain => 1,
                H::Seed(s) => s.wrapping_add(1),
            },
            json::Json::Str(s) => h.bytes(s.as_bytes()),
            json::Json::Number(n) => hash_numeric(&Bson::String(n.clone()), h)?,
            json::Json::Bool(b) => match h {
                H::Seed(s) if s != 0 => h.uint32(u32::from(*b)),
                _ => {
                    if *b {
                        2
                    } else {
                        4
                    }
                }
            },
            _ => 0,
        };
        *hash = rotate(*hash) ^ tmp;
        Ok(())
    };
    fn walk(
        v: &json::Json,
        hash: &mut u64,
        flag: &dyn Fn(u32) -> u64,
        scalar: &dyn Fn(&json::Json, &mut u64) -> Result<()>,
    ) -> Result<()> {
        match v {
            json::Json::Array(items) => {
                *hash ^= flag(0x4000_0000);
                for i in items {
                    walk(i, hash, flag, scalar)?;
                }
            }
            json::Json::Object(pairs) => {
                *hash ^= flag(0x2000_0000);
                let mut kept: Vec<(&String, &json::Json)> = Vec::new();
                for (k, val) in pairs {
                    match kept.iter_mut().find(|(existing, _)| *existing == k) {
                        Some(slot) => slot.1 = val,
                        None => kept.push((k, val)),
                    }
                }
                kept.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(b.0)));
                for (k, val) in kept {
                    scalar(&json::Json::Str(k.clone()), hash)?;
                    walk(val, hash, flag, scalar)?;
                }
            }
            other => scalar(other, hash)?,
        }
        Ok(())
    }
    let mut hash = 0u64;
    let root_scalar = !matches!(j, json::Json::Array(_) | json::Json::Object(_));
    if root_scalar {
        hash ^= flag(ARRAY);
        scalar(&j, &mut hash)?;
    } else {
        walk(&j, &mut hash, &flag, &scalar)?;
    }
    let _ = OBJECT;
    Ok(hash)
}

/// The hash functions this module answers, as `(name, result type)`.
pub(crate) fn result_type(name: &str, nargs: usize) -> Option<&'static str> {
    let extended = name.ends_with("extended");
    let base = match name {
        "hash_array" | "hash_range" | "hash_multirange" | "hash_numeric" | "hashbpchar"
        | "hashchar" | "hashfloat4" | "hashfloat8" | "hashinet" | "hashint2" | "hashint4"
        | "hashint8" | "hashmacaddr" | "hashmacaddr8" | "hashname" | "hashoid" | "hashtext"
        | "hashtid" | "interval_hash" | "pg_lsn_hash" | "time_hash" | "timestamp_hash"
        | "timetz_hash" | "uuid_hash" | "jsonb_hash" => true,
        n if extended => result_type(
            n.trim_end_matches("_extended").trim_end_matches("extended"),
            1,
        )
        .is_some(),
        _ => false,
    };
    match (base, extended, nargs) {
        (true, false, 1) => Some("int4"),
        (true, true, 2) => Some("int8"),
        _ => None,
    }
}

/// The type `name` hashes, or `None` for a polymorphic one.
fn declared(name: &str) -> Option<&'static str> {
    Some(match name {
        "hash_numeric" => "numeric",
        "hashbpchar" => "bpchar",
        "hashchar" => "\"char\"",
        "hashfloat4" => "float4",
        "hashfloat8" => "float8",
        "hashinet" => "inet",
        "hashint2" => "int2",
        "hashint4" => "int4",
        "hashint8" => "int8",
        "hashmacaddr" => "macaddr",
        "hashmacaddr8" => "macaddr8",
        "hashname" => "name",
        "hashoid" => "oid",
        "hashtext" => "text",
        "hashtid" => "tid",
        "interval_hash" => "interval",
        "pg_lsn_hash" => "pg_lsn",
        "time_hash" => "time",
        "timestamp_hash" => "timestamp",
        "timetz_hash" => "timetz",
        "uuid_hash" => "uuid",
        _ => return None,
    })
}

/// `name(args)`, the first argument's static type being `ty`.
pub(crate) fn call(name: &str, args: &[Bson], ty: &str) -> Option<Result<Bson>> {
    let out_type = result_type(name, args.len())?;
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    let base = name
        .trim_end_matches("_extended")
        .trim_end_matches("extended");
    let h = if out_type == "int8" {
        match &args[1] {
            Bson::Int64(s) => H::Seed(*s as u64),
            Bson::Int32(s) => H::Seed(i64::from(*s) as u64),
            other => match value_text(other).trim().parse::<i64>() {
                Ok(s) => H::Seed(s as u64),
                Err(_) => return Some(Err(Error::Internal("hash seed".into()))),
            },
        }
    } else {
        H::Plain
    };
    let hash = match base {
        "hash_array" => {
            let element = ty.strip_suffix("[]").unwrap_or("");
            hash_array(&args[0], element, h)
        }
        "hash_range" => range::range_element(ty)
            .ok_or_else(|| Error::Unsupported(format!("hash_range({ty})")))
            .and_then(|(element, _)| {
                let r = range::from_text(&value_text(&args[0]), ty)?;
                range_hash(&r, &element, h)
            }),
        "hash_multirange" => (|| {
            let member = range::multirange_member(ty)
                .ok_or_else(|| Error::Unsupported(format!("hash_multirange({ty})")))?;
            let (element, _) = range::range_element(&member)
                .ok_or_else(|| Error::Unsupported(format!("hash_multirange({ty})")))?;
            let mut result: u64 = 1;
            for r in range::multirange_from_text(&value_text(&args[0]), ty)? {
                let rh = range_hash(&r, &element, h)?;
                result = match h {
                    H::Plain => u64::from(
                        (result as u32)
                            .wrapping_shl(5)
                            .wrapping_sub(result as u32)
                            .wrapping_add(rh as u32),
                    ),
                    H::Seed(_) => result.wrapping_shl(5).wrapping_sub(result).wrapping_add(rh),
                };
            }
            Ok(result)
        })(),
        "jsonb_hash" => jsonb_hash(&value_text(&args[0]), h),
        other => {
            let d = declared(other).unwrap_or("text");
            hash_value(&args[0], d, h)
        }
    };
    Some(hash.map(|v| match out_type {
        "int4" => Bson::Int32(v as u32 as i32),
        _ => Bson::Int64(v as i64),
    }))
}
