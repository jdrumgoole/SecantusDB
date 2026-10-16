//! The B-tree and GIN comparison functions whose answer is a byte
//! comparison's, called by name -- `byteacmp`, `bitcmp`, `btnamecmp`,
//! `network_cmp`, ... -- as PostgreSQL 15.19 computes them: a `memcmp` /
//! `strncmp` over the bytes, the difference of the first two bytes that
//! differ (what both macOS's and glibc's return), then a rule of the
//! function's own for equal prefixes (`varlena.c`, `varbit.c`, `name.c`,
//! `network.c`, `tsvector_op.c`, `jsonb_gin.c`).
//!
//! Not here: `bttextcmp`, `bpcharcmp` and `jsonb_cmp`, which compare under
//! the database's collation through `strcoll`, whose magnitude is the
//! platform's locale data; and `tsvector_cmp` / `tsquery_cmp`, which compare
//! their types' internal storage layouts.

use super::*;

/// `memcmp`: the difference of the first differing bytes, else 0.
fn memcmp(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .find(|(x, y)| x != y)
        .map_or(0, |(x, y)| i32::from(*x) - i32::from(*y))
}

/// A `memcmp` over the shorter length, then the shorter first (-1 / 1):
/// `byteacmp`, `varstr_cmp` under `C`, `internal_text_pattern_compare`,
/// `tsCompareString`.
fn memcmp_then_length(a: &[u8], b: &[u8]) -> i32 {
    let n = a.len().min(b.len());
    match memcmp(&a[..n], &b[..n]) {
        0 if a.len() == b.len() => 0,
        0 if a.len() < b.len() => -1,
        0 => 1,
        c => c,
    }
}

/// `bit_cmp`: the packed bytes (zero-padded), then the bit length.
fn bit_cmp(a: &str, b: &str) -> i32 {
    let pack = |s: &str| -> Vec<u8> {
        s.as_bytes()
            .chunks(8)
            .map(|c| {
                c.iter().enumerate().fold(0u8, |acc, (i, bit)| {
                    acc | (u8::from(*bit == b'1') << (7 - i))
                })
            })
            .collect()
    };
    let (pa, pb) = (pack(a), pack(b));
    let n = pa.len().min(pb.len());
    match memcmp(&pa[..n], &pb[..n]) {
        0 if a.len() == b.len() => 0,
        0 if a.len() < b.len() => -1,
        0 => 1,
        c => c,
    }
}

/// `bitncmp`: the first `n` bits, a whole byte at a time and then bit by
/// bit.
fn bitncmp(l: &[u8], r: &[u8], n: usize) -> i32 {
    let whole = n / 8;
    let x = memcmp(&l[..whole.min(l.len())], &r[..whole.min(r.len())]);
    if x != 0 || n.is_multiple_of(8) {
        return x;
    }
    let (mut lb, mut rb) = (
        l.get(whole).copied().unwrap_or(0),
        r.get(whole).copied().unwrap_or(0),
    );
    for _ in 0..n % 8 {
        let (lh, rh) = (lb & 0x80 != 0, rb & 0x80 != 0);
        if lh != rh {
            return if lh { 1 } else { -1 };
        }
        lb <<= 1;
        rb <<= 1;
    }
    0
}

/// `network_cmp_internal` over `(family, bits, address)`.
fn network_cmp(a: &[u8], b: &[u8]) -> i32 {
    let (fa, ba, aa) = (i32::from(a[0]), usize::from(a[1]), &a[2..]);
    let (fb, bb, ab) = (i32::from(b[0]), usize::from(b[1]), &b[2..]);
    if fa != fb {
        return fa - fb;
    }
    let order = bitncmp(aa, ab, ba.min(bb));
    if order != 0 {
        return order;
    }
    let order = ba as i32 - bb as i32;
    if order != 0 {
        return order;
    }
    bitncmp(aa, ab, aa.len() * 8)
}

/// The result type of `name` over `nargs`, when it is one of these.
pub(crate) fn result_type(name: &str, nargs: usize) -> Option<&'static str> {
    (nargs == 2
        && matches!(
            name,
            "byteacmp"
                | "bitcmp"
                | "varbitcmp"
                | "btnamecmp"
                | "btnametextcmp"
                | "bttextnamecmp"
                | "bttext_pattern_cmp"
                | "btbpchar_pattern_cmp"
                | "network_cmp"
                | "gin_cmp_tslexeme"
                | "gin_compare_jsonb"
        ))
    .then_some("int4")
}

/// `name(a, b)`, when it is one of these; each is strict.
pub(crate) fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    result_type(name, args.len())?;
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    let text = |v: &Bson| value_text(v);
    Some(
        (|| -> Result<i32> {
            Ok(match name {
                "byteacmp" => {
                    memcmp_then_length(&bytea::parse(&args[0])?, &bytea::parse(&args[1])?)
                }
                "bitcmp" | "varbitcmp" => bit_cmp(&text(&args[0]), &text(&args[1])),
                // A name compares as C strings over NAMEDATALEN, its NUL padding
                // included: `strncmp`.
                "btnamecmp" => {
                    let mut a = text(&args[0]).into_bytes();
                    let mut b = text(&args[1]).into_bytes();
                    a.resize(64, 0);
                    b.resize(64, 0);
                    memcmp(&a, &b)
                }
                "btnametextcmp" | "bttextnamecmp" | "bttext_pattern_cmp" | "gin_cmp_tslexeme"
                | "gin_compare_jsonb" => {
                    memcmp_then_length(text(&args[0]).as_bytes(), text(&args[1]).as_bytes())
                }
                "btbpchar_pattern_cmp" => memcmp_then_length(
                    text(&args[0]).trim_end_matches(' ').as_bytes(),
                    text(&args[1]).trim_end_matches(' ').as_bytes(),
                ),
                _ => {
                    let a =
                        crate::hashfn::inet_bytes(&text(&cast_value(args[0].clone(), "inet")?))?;
                    let b =
                        crate::hashfn::inet_bytes(&text(&cast_value(args[1].clone(), "inet")?))?;
                    network_cmp(&a, &b)
                }
            })
        })()
        .map(Bson::Int32),
    )
}
