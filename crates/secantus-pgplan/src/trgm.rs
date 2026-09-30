//! `pg_trgm`: trigram similarity, transcribed from `contrib/pg_trgm`
//! (`trgm_op.c`).
//!
//! A string's trigrams come from its WORDS -- runs of alphanumeric
//! characters, lowercased -- each padded with two spaces in front and one
//! behind, so `cat` gives `"  c"`, `" ca"`, `"cat"`, `"at "`. A trigram with
//! a multibyte character is stored compactly as three bytes of the legacy
//! CRC32 of its bytes, which is why `show_trgm('ü...')` prints `0x......`.
//! Trigrams order as `char[3]` compared as SIGNED chars (`CMPCHAR`).
//!
//! `similarity` is |common| / |union| of the two trigram sets;
//! `word_similarity` the best of that between the first string and a
//! contiguous extent of the second's trigrams (`iterate_word_similarity`),
//! and `strict_word_similarity` the same with extents on word boundaries.

use bson::Bson;

type Trgm = [u8; 3];

const LPADDING: usize = 2;
const RPADDING: usize = 1;

/// PostgreSQL's legacy CRC32: the reflected table driven by the NORMAL
/// (MSB-first) loop, a long-standing quirk kept for on-disk compatibility.
fn legacy_crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        let idx = ((crc >> 24) ^ u32::from(*b)) & 0xFF;
        crc = table[idx as usize] ^ (crc << 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// One trigram from three characters: their bytes when that is three,
/// else the compact CRC form.
fn make_trigram(chars: &[char]) -> Trgm {
    let mut buf = String::new();
    for c in chars {
        buf.push(*c);
    }
    let bytes = buf.as_bytes();
    if bytes.len() == 3 {
        [bytes[0], bytes[1], bytes[2]]
    } else {
        let crc = legacy_crc32(bytes).to_le_bytes();
        [crc[0], crc[1], crc[2]]
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The words of `s`, lowercased.
fn words(s: &str) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    for c in s.chars() {
        if is_word_char(c) {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `generate_trgm_only`: every trigram in order, duplicates kept, with
/// `(left, right)` word-boundary flags for the strict variant.
fn positional(s: &str) -> Vec<(Trgm, bool, bool)> {
    let mut out = Vec::new();
    for w in words(s) {
        let mut padded: Vec<char> = vec![' '; LPADDING];
        padded.extend(&w);
        padded.extend(std::iter::repeat_n(' ', RPADDING));
        let n = padded.len() - 2;
        for i in 0..n {
            out.push((make_trigram(&padded[i..i + 3]), i == 0, i + 1 == n));
        }
    }
    out
}

fn cmp_trgm(a: &Trgm, b: &Trgm) -> std::cmp::Ordering {
    let s = |t: &Trgm| [t[0] as i8, t[1] as i8, t[2] as i8];
    s(a).cmp(&s(b))
}

/// `generate_trgm`: the sorted, unique trigram set.
fn trigram_set(s: &str) -> Vec<Trgm> {
    let mut t: Vec<Trgm> = positional(s).into_iter().map(|(t, _, _)| t).collect();
    t.sort_by(cmp_trgm);
    t.dedup();
    t
}

fn calc_sml(count: usize, len1: usize, len2: usize) -> f32 {
    let denom = len1 + len2 - count;
    if denom == 0 {
        0.0
    } else {
        count as f32 / denom as f32
    }
}

/// `similarity(a, b)`.
pub fn similarity(a: &str, b: &str) -> f32 {
    let (x, y) = (trigram_set(a), trigram_set(b));
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < x.len() && j < y.len() {
        match cmp_trgm(&x[i], &y[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    calc_sml(common, x.len(), y.len())
}

/// `word_similarity` / `strict_word_similarity` (`calc_word_similarity`).
pub fn word_similarity(a: &str, b: &str, strict: bool) -> f32 {
    let t1 = positional(a);
    let t2 = positional(b);
    let (len1, len2) = (t1.len(), t2.len());
    if len1 == 0 || len2 == 0 {
        return 0.0;
    }
    // Positional trigrams: the first string's with index -1, the second's
    // with their position; sorted by (trigram, index).
    let mut ptrg: Vec<(Trgm, i64)> = t1
        .iter()
        .map(|(t, _, _)| (*t, -1))
        .chain(t2.iter().enumerate().map(|(i, (t, _, _))| (*t, i as i64)))
        .collect();
    ptrg.sort_by(|x, y| cmp_trgm(&x.0, &y.0).then(x.1.cmp(&y.1)));
    let len = len1 + len2;
    let mut trg2indexes = vec![0usize; len2];
    let mut found = vec![false; len];
    let mut ulen1 = 0usize;
    let mut j = 0usize;
    for i in 0..len {
        if i > 0 && cmp_trgm(&ptrg[i - 1].0, &ptrg[i].0) != std::cmp::Ordering::Equal {
            if found[j] {
                ulen1 += 1;
            }
            j += 1;
        }
        if ptrg[i].1 >= 0 {
            trg2indexes[ptrg[i].1 as usize] = j;
        } else {
            found[j] = true;
        }
    }
    if found[j] {
        ulen1 += 1;
    }
    let left = |i: usize| t2[i].1;
    let right = |i: usize| t2[i].2;
    // `iterate_word_similarity`.
    let mut lastpos: Vec<i64> = vec![-1; len];
    let (mut ulen2, mut count) = (0usize, 0usize);
    let mut lower: i64 = if strict { 0 } else { -1 };
    let mut smlr_max = 0.0f32;
    for i in 0..len2 {
        let trgindex = trg2indexes[i];
        if lower >= 0 || found[trgindex] {
            if lastpos[trgindex] < 0 {
                ulen2 += 1;
                if found[trgindex] {
                    count += 1;
                }
            }
            lastpos[trgindex] = i as i64;
        }
        let bound = if strict { right(i) } else { found[trgindex] };
        if bound {
            let upper = i as i64;
            if lower == -1 {
                lower = i as i64;
                ulen2 = 1;
            }
            let mut smlr_cur = calc_sml(count, ulen1, ulen2);
            let (mut tmp_count, mut tmp_ulen2) = (count, ulen2);
            let prev_lower = lower;
            let mut tmp_lower = lower;
            while tmp_lower <= upper {
                if !strict || left(tmp_lower as usize) {
                    let smlr_tmp = calc_sml(tmp_count, ulen1, tmp_ulen2);
                    if smlr_tmp > smlr_cur {
                        smlr_cur = smlr_tmp;
                        ulen2 = tmp_ulen2;
                        lower = tmp_lower;
                        count = tmp_count;
                    }
                }
                let tmp_trgindex = trg2indexes[tmp_lower as usize];
                if lastpos[tmp_trgindex] == tmp_lower {
                    tmp_ulen2 = tmp_ulen2.saturating_sub(1);
                    if found[tmp_trgindex] {
                        tmp_count = tmp_count.saturating_sub(1);
                    }
                }
                tmp_lower += 1;
            }
            smlr_max = smlr_max.max(smlr_cur);
            let mut t = prev_lower;
            while t < lower {
                let tmp_trgindex = trg2indexes[t as usize];
                if lastpos[tmp_trgindex] == t {
                    lastpos[tmp_trgindex] = -1;
                }
                t += 1;
            }
        }
    }
    smlr_max
}

/// `show_trgm(text)`: the trigram set as text, a compact one as `0x......`.
pub fn show_trgm(s: &str) -> Bson {
    Bson::Array(
        trigram_set(s)
            .into_iter()
            .map(|t| {
                if t.iter().all(|b| b.is_ascii() && !b.is_ascii_control()) {
                    Bson::String(String::from_utf8_lossy(&t).into_owned())
                } else {
                    Bson::String(format!(
                        "0x{:06x}",
                        (u32::from(t[0]) << 16) | (u32::from(t[1]) << 8) | u32::from(t[2])
                    ))
                }
            })
            .collect(),
    )
}

/// The GUC thresholds, from the session (or their defaults).
pub fn threshold(name: &str) -> f64 {
    let default = match name {
        "pg_trgm.word_similarity_threshold" => 0.6,
        "pg_trgm.strict_word_similarity_threshold" => 0.5,
        _ => 0.3,
    };
    crate::session_setting(name)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

pub const FUNCTIONS: &[&str] = &[
    "similarity",
    "show_trgm",
    "word_similarity",
    "strict_word_similarity",
    "show_limit",
    "similarity_dist",
    "word_similarity_dist_op",
];

pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name) && crate::extension_installed("pg_trgm")
}

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "show_trgm" => "text[]",
        "similarity" | "word_similarity" | "strict_word_similarity" | "show_limit" => "float4",
        _ => return None,
    })
}

pub fn call(name: &str, args: &[Bson]) -> crate::Result<Bson> {
    let text = |i: usize| crate::value_text(&args[i]);
    let f = |v: f32| Bson::Double(f64::from(v));
    Ok(match name {
        "show_limit" => f(threshold("pg_trgm.similarity_threshold") as f32),
        _ if args.iter().any(|a| *a == Bson::Null) => Bson::Null,
        "similarity" => f(similarity(&text(0), &text(1))),
        "show_trgm" => show_trgm(&text(0)),
        "word_similarity" => f(word_similarity(&text(0), &text(1), false)),
        "strict_word_similarity" => f(word_similarity(&text(0), &text(1), true)),
        _ => return Err(crate::Error::Unsupported(format!("function {name}"))),
    })
}

/// A `pg_trgm` operator over two texts, when `op` is one.
pub fn operator(op: &str, a: &str, b: &str) -> Option<Bson> {
    if !crate::extension_installed("pg_trgm") {
        return None;
    }
    let f = |v: f32| Bson::Double(f64::from(v));
    Some(match op {
        "%" => {
            Bson::Boolean(f64::from(similarity(a, b)) >= threshold("pg_trgm.similarity_threshold"))
        }
        "<->" => f(1.0 - similarity(a, b)),
        "<%" => Bson::Boolean(
            f64::from(word_similarity(a, b, false))
                >= threshold("pg_trgm.word_similarity_threshold"),
        ),
        "%>" => Bson::Boolean(
            f64::from(word_similarity(b, a, false))
                >= threshold("pg_trgm.word_similarity_threshold"),
        ),
        "<<%" => Bson::Boolean(
            f64::from(word_similarity(a, b, true))
                >= threshold("pg_trgm.strict_word_similarity_threshold"),
        ),
        "%>>" => Bson::Boolean(
            f64::from(word_similarity(b, a, true))
                >= threshold("pg_trgm.strict_word_similarity_threshold"),
        ),
        "<<->" => f(1.0 - word_similarity(a, b, false)),
        "<->>" => f(1.0 - word_similarity(b, a, false)),
        "<<<->" => f(1.0 - word_similarity(a, b, true)),
        "<->>>" => f(1.0 - word_similarity(b, a, true)),
        _ => return None,
    })
}
