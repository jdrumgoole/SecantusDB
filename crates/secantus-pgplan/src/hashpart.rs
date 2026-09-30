//! `PARTITION BY HASH` routing, as PostgreSQL computes it -- so a row lands
//! in the partition PostgreSQL would put it in, which `tableoid` and a read
//! of one partition both expose.
//!
//! Transcribed from `src/common/hashfn.c` (Bob Jenkins' lookup3, in its
//! 64-bit "extended" form), the types' `*extended` hash support functions,
//! and `partbounds.c`'s `compute_partition_hash_value`: each non-NULL key is
//! hashed with `HASH_PARTITION_SEED` and folded with `hash_combine64`; a row
//! belongs to the partition whose `(modulus, remainder)` has
//! `hash % modulus == remainder`.

use bson::Bson;

/// `partition.h`'s `HASH_PARTITION_SEED`.
const HASH_PARTITION_SEED: u64 = 0x7A5B_2236_7996_DCFD;

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(4);
    *b = b.wrapping_add(*a);
}

fn finalize(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(14));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(11));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(25));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(16));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(4));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(14));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(24));
}

fn seeded(len: u32, seed: u64) -> (u32, u32, u32) {
    let init = 0x9e37_79b9u32.wrapping_add(len).wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (init, init, init);
    if seed != 0 {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix(&mut a, &mut b, &mut c);
    }
    (a, b, c)
}

/// `hash_bytes_uint32_extended`.
pub fn hash_uint32_extended(k: u32, seed: u64) -> u64 {
    let (mut a, mut b, mut c) = seeded(4, seed);
    a = a.wrapping_add(k);
    finalize(&mut a, &mut b, &mut c);
    (u64::from(b) << 32) | u64::from(c)
}

/// `hash_bytes_extended` (the little-endian, byte-at-a-time path, which
/// computes the same value as the word-aligned one).
pub fn hash_bytes_extended(k: &[u8], seed: u64) -> u64 {
    let (mut a, mut b, mut c) = seeded(k.len() as u32, seed);
    let word = |s: &[u8]| {
        u32::from(s[0]) | (u32::from(s[1]) << 8) | (u32::from(s[2]) << 16) | (u32::from(s[3]) << 24)
    };
    let mut rest = k;
    while rest.len() >= 12 {
        a = a.wrapping_add(word(&rest[0..4]));
        b = b.wrapping_add(word(&rest[4..8]));
        c = c.wrapping_add(word(&rest[8..12]));
        mix(&mut a, &mut b, &mut c);
        rest = &rest[12..];
    }
    let byte = |i: usize, shift: u32| u32::from(rest[i]) << shift;
    let n = rest.len();
    // Every case falls through to the ones below it; the lowest byte of c
    // is reserved for the length.
    if n >= 11 {
        c = c.wrapping_add(byte(10, 24));
    }
    if n >= 10 {
        c = c.wrapping_add(byte(9, 16));
    }
    if n >= 9 {
        c = c.wrapping_add(byte(8, 8));
    }
    if n >= 8 {
        b = b.wrapping_add(byte(7, 24));
    }
    if n >= 7 {
        b = b.wrapping_add(byte(6, 16));
    }
    if n >= 6 {
        b = b.wrapping_add(byte(5, 8));
    }
    if n >= 5 {
        b = b.wrapping_add(byte(4, 0));
    }
    if n >= 4 {
        a = a.wrapping_add(byte(3, 24));
    }
    if n >= 3 {
        a = a.wrapping_add(byte(2, 16));
    }
    if n >= 2 {
        a = a.wrapping_add(byte(1, 8));
    }
    if n >= 1 {
        a = a.wrapping_add(byte(0, 0));
    }
    finalize(&mut a, &mut b, &mut c);
    (u64::from(b) << 32) | u64::from(c)
}

/// `hash_combine64`.
fn hash_combine64(a: u64, b: u64) -> u64 {
    a ^ b
        .wrapping_add(0x49a0_f4dd_15e5_a8e3)
        .wrapping_add(a << 54)
        .wrapping_add(a >> 7)
}

/// `hashint8extended`: an int8 folded so a value in int4 range hashes as
/// the int4 does.
fn hash_int8(v: i64, seed: u64) -> u64 {
    let mut lo = v as u32;
    let hi = (v >> 32) as u32;
    lo ^= if v >= 0 { hi } else { !hi };
    hash_uint32_extended(lo, seed)
}

fn int_of(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
        _ => None,
    }
}

/// One key value's extended hash, by its column's type; `None` for a type
/// whose hash this does not reproduce.
fn hash_value(v: &Bson, ty: &str) -> Option<u64> {
    let seed = HASH_PARTITION_SEED;
    Some(match ty {
        "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" => hash_int8(int_of(v)?, seed),
        "oid" => hash_uint32_extended(int_of(v)? as u32, seed),
        "bool" | "boolean" => hash_uint32_extended(u32::from(v.as_bool()?), seed),
        // Days since 2000-01-01, hashed as an int4.
        "date" => {
            let text = crate::value_text(v);
            let d = chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()?;
            let epoch = chrono::NaiveDate::from_ymd_opt(2000, 1, 1)?;
            hash_int8((d - epoch).num_days(), seed)
        }
        // Microseconds since 2000-01-01, hashed as an int8.
        "timestamp" | "timestamptz" => {
            let micros = crate::instant_micros(v)?;
            hash_int8(micros - 946_684_800_000_000, seed)
        }
        "float4" | "float8" | "real" | "double precision" => {
            let f = match v {
                Bson::Double(d) => *d,
                other => int_of(other)? as f64,
            };
            // `hashfloat8extended`: both zeroes hash as the seed; every NaN
            // as one NaN.
            if f == 0.0 {
                seed
            } else if f.is_nan() {
                hash_bytes_extended(&f64::NAN.to_le_bytes(), seed)
            } else {
                hash_bytes_extended(&f.to_le_bytes(), seed)
            }
        }
        "text" | "varchar" | "name" | "character varying" => {
            hash_bytes_extended(crate::value_text(v).as_bytes(), seed)
        }
        // `bpchar` hashes without its trailing blanks.
        "bpchar" | "character" | "char" => {
            hash_bytes_extended(crate::value_text(v).trim_end_matches(' ').as_bytes(), seed)
        }
        "uuid" => {
            let hex: String = crate::value_text(v).chars().filter(|c| *c != '-').collect();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
                .collect::<Option<_>>()?;
            hash_bytes_extended(&bytes, seed)
        }
        _ => return None,
    })
}

/// Whether PostgreSQL's hash support for this key type is reproduced here.
pub fn supported_type(ty: &str) -> bool {
    matches!(
        ty,
        "int2"
            | "int4"
            | "int8"
            | "oid"
            | "bool"
            | "date"
            | "timestamp"
            | "timestamptz"
            | "float4"
            | "float8"
            | "text"
            | "varchar"
            | "name"
            | "bpchar"
            | "uuid"
    )
}

/// `compute_partition_hash_value` over `(value, type)` keys, NULLs skipped.
pub fn row_hash(keys: &[(Bson, String)]) -> Option<u64> {
    let mut hash = 0u64;
    for (v, ty) in keys {
        if *v == Bson::Null {
            continue;
        }
        hash = hash_combine64(hash, hash_value(v, ty)?);
    }
    Some(hash)
}

/// The internal `secantus_hash_partition(modulus, remainder, type, value,
/// type, value, ...)` a hash partition's condition calls.
pub fn satisfies(args: &[Bson]) -> crate::Result<Bson> {
    let int = |v: &Bson| int_of(v).unwrap_or(0);
    let (modulus, remainder) = (int(&args[0]), int(&args[1]));
    let keys: Vec<(Bson, String)> = args[2..]
        .chunks(2)
        .filter_map(|p| match p {
            [t, v] => Some((v.clone(), crate::value_text(t))),
            _ => None,
        })
        .collect();
    let hash = row_hash(&keys)
        .ok_or_else(|| crate::Error::Unsupported("hash partitioning over this key type".into()))?;
    Ok(Bson::Boolean(
        modulus > 0 && hash % modulus as u64 == remainder as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup3_known_values() {
        // hash_bytes_extended over no bytes with no seed is the finalised
        // initial state; with a seed it must differ.
        assert_ne!(hash_bytes_extended(b"", 0), hash_bytes_extended(b"", 1));
        // hashint8 of a value in int4 range equals hashint4's.
        assert_eq!(hash_int8(-5, 7), hash_uint32_extended((-5i32) as u32, 7));
        assert_eq!(hash_int8(42, 7), hash_uint32_extended(42, 7));
    }
}
