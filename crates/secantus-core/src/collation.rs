//! Collation-aware string normalisation — Rust counterpart of
//! `secantus.collation`, intentionally scoped to what is **version-independent**.
//!
//! The Python implementation leans on `unicodedata` (NFKD + `category('Mn')`)
//! and `str.casefold()`, both of which depend on the Unicode version bundled
//! with the running CPython. Reproducing those in Rust would risk drift on rare
//! characters. So this module handles only the cases where the transformation
//! is unambiguous regardless of Unicode version:
//!
//! * **ASCII strings** — accent stripping is a no-op (ASCII has no combining
//!   marks / decompositions) and `casefold()` is exactly ASCII-lowercasing
//!   (no special cases like ß→ss live in ASCII).
//! * **No active transform** (strength 3, no `caseLevel` effect) — the string
//!   passes through unchanged, so *any* string is fine.
//!
//! Anything else — a non-ASCII string under an accent/case-insensitive
//! collation, or `numericOrdering` (needs Python's `\d`/`isdigit` + tuple
//! ordering) — returns `None` ("defer to Python"), and the caller falls back to
//! the pure-Python collation path, which is authoritative.

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

use bson::{Bson, Document};

pub struct Collation {
    pub strength: i32,
    pub case_level: bool,
    pub numeric_ordering: bool,
    /// `"upper"` flips the tertiary (case) level so capitals sort first.
    pub case_first_upper: bool,
    /// French ordering: the secondary (accent) level is compared from the END
    /// of the string, which is what makes `coté` sort before `côte`.
    pub backwards: bool,
}

impl Collation {
    fn case_insensitive(&self) -> bool {
        self.strength <= 2 && !self.case_level
    }

    fn accent_insensitive(&self) -> bool {
        self.strength <= 1
    }
}

/// Parse the wire form `{strength, caseLevel, numericOrdering}`. An empty
/// document means "no collation" (`None`).
pub fn parse(d: &Document) -> Option<Collation> {
    if d.is_empty() {
        return None;
    }
    let strength = match d.get("strength") {
        Some(Bson::Int32(n)) => *n,
        Some(Bson::Int64(n)) => *n as i32,
        _ => 3,
    };
    let flag = |k: &str| matches!(d.get(k), Some(Bson::Boolean(true)));
    Some(Collation {
        strength,
        case_level: flag("caseLevel"),
        numeric_ordering: flag("numericOrdering"),
        case_first_upper: matches!(d.get("caseFirst"), Some(Bson::String(s)) if s == "upper"),
        backwards: flag("backwards"),
    })
}

/// Python's `unicodedata.category(c) == "Mn"` -- NONSPACING mark only. Not
/// `unicode_normalization::char::is_combining_mark`, which is true for all of
/// `M*` (Mc and Me as well, measured 2026-09-07): that strips a Devanagari
/// vowel sign such as U+093E, which `_strip_accents` KEEPS, so the two engines
/// would disagree on any Indic string.
fn is_mn(c: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    c.general_category() == GeneralCategory::NonspacingMark
}

/// Python's `unicodedata.combining(ch)` -- nonzero canonical combining class.
/// `sort_levels` uses THIS, not the `Mn` test above, and the two are not the
/// same set (an `Mn` with ccc 0 exists, e.g. U+0900). Mirror each site exactly.
fn is_ccc_mark(c: char) -> bool {
    unicode_normalization::char::canonical_combining_class(c) != 0
}

/// Normalise a string under the collation, or `None` if the case can't be
/// reproduced version-independently (non-ASCII transform, or numericOrdering).
fn normalize(s: &str, c: &Collation) -> Option<String> {
    if c.numeric_ordering {
        return None; // needs Python's digit-run tuple ordering
    }
    // Non-ASCII used to `return None` (defer) here, exactly as it did in
    // `normalize_index_bytes` -- and with the same consequence on the Rust
    // server, which has no Python behind a defer: every collated EQUALITY or
    // RANGE query against a non-ASCII string answered `2 BadValue: query uses
    // a construct the Rust server does not support`. Fixing only the index
    // path left this one live; the tests caught it.
    normalize_index_bytes(s, c).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Collation-aware string equality, or `None` to defer to Python.
pub fn equal(a: &str, b: &str, c: &Collation) -> Option<bool> {
    Some(normalize(a, c)? == normalize(b, c)?)
}

/// Collation-aware string ordering, or `None` to defer to Python. Normalised
/// ASCII strings compare by byte order, which equals Python's codepoint
/// ordering of the same normalised strings.
pub fn compare(a: &str, b: &str, c: &Collation) -> Option<Ordering> {
    Some(normalize(a, c)?.cmp(&normalize(b, c)?))
}

/// Normalised UTF-8 bytes for index-key encoding (`normalize_for_index_bytes`),
/// or `None` to defer. Differs from `normalize` in one way: a `numericOrdering`
/// collation has `supports_index_encoding == false`, so Python's `_encode_string`
/// skips normalisation and emits the **raw** UTF-8 — i.e. numericOrdering is an
/// identity transform here (not a defer, as it is for query comparison).
pub fn normalize_index_bytes(s: &str, c: &Collation) -> Option<Vec<u8>> {
    if c.numeric_ordering {
        return Some(s.as_bytes().to_vec()); // !supports_index_encoding -> raw
    }
    let (accent, case) = (c.accent_insensitive(), c.case_insensitive());
    if !accent && !case {
        return Some(s.as_bytes().to_vec()); // identity
    }
    // Non-ASCII used to `return None` here, meaning "defer to the pure engine".
    // That is right on the Python server and WRONG on the Rust one, which has
    // no Python behind a defer: it surfaced as
    // `2 BadValue: an indexed value is of a type the Rust server does not
    // support` for any case- or accent-insensitive query or sort touching a
    // non-ASCII character -- `á`, `ß`, `日` alike. Measured against 8.2.11 on
    // 2026-09-07, where mongod (and the Python server) answer normally.
    //
    // The order below is `collation.py`'s `normalize_for_index_bytes`, and it
    // MUST stay identical: the two engines' bytes are compared to each other by
    // the parity suite, and an index written by one server is read by the other.
    let mut out = s.to_string();
    if accent {
        // NFKD splits an accented character into base + combining marks, and
        // dropping the marks leaves the base -- `unicodedata.category(c) !=
        // "Mn"` in the Python.
        out = out.nfkd().filter(|c| !is_mn(*c)).collect();
    }
    if case {
        out = case_fold(&out);
    }
    Some(out.into_bytes())
}

/// Unicode case folding, as Python's `str.casefold` does it.
///
/// NOT `to_lowercase`: folding is the case-insensitive-comparison mapping and
/// is more aggressive. The difference is load-bearing here -- `ß` folds to
/// `ss`, and mongod sorts `["ß", "s", "t"]` as `["s", "ß", "t"]`, which only
/// comes out right if `ß` compares as `ss`. `to_lowercase` leaves `ß` alone and
/// sorts it after `t`.
///
/// `to_lowercase` supplies the common mapping; the table is Unicode's FULL
/// case-folding entries whose result differs from lowercasing (CaseFolding.txt,
/// status `F`), restricted to the ones reachable from BSON text. Kept explicit
/// rather than pulling a second crate for a handful of characters.
fn case_fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'ß' => out.push_str("ss"),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            // Greek final sigma folds to the ordinary one, so "ΟΔΟΣ" and
            // "οδός" compare equal on their last letter.
            'ς' => out.push('σ'),
            'ΐ' => out.push_str("\u{3b9}\u{308}\u{301}"),
            'ΰ' => out.push_str("\u{3c5}\u{308}\u{301}"),
            other => {
                for lowered in other.to_lowercase() {
                    out.push(lowered);
                }
            }
        }
    }
    out
}

//: The order combining marks sort in. NOT codepoint order -- acute sorts before
//: grave and the codepoints run the other way -- so this is a measured table,
//: identical to `collation.py`'s `_MARK_ORDER`. Both engines must agree: they
//: are compared to each other by the parity suite.
const MARK_ORDER: [u32; 20] = [
    0x332, // low line
    0x301, // acute
    0x300, // grave
    0x306, // breve
    0x302, // circumflex
    0x30C, // caron
    0x30A, // ring above
    0x308, // diaeresis
    0x30B, // double acute
    0x303, // tilde
    0x307, // dot above
    0x327, // cedilla
    0x328, // ogonek
    0x304, // macron
    0x309, // hook above
    0x30F, // double grave
    0x311, // inverted breve
    0x323, // dot below
    0x326, // comma below
    0x331, // macron below
];

/// Secondary weight for one combining mark: `(table rank, codepoint)`.
/// An unlisted mark sorts after every listed one, by codepoint -- which keeps
/// the key total and deterministic.
fn mark_weight(cp: u32) -> (u32, u32) {
    match MARK_ORDER.iter().position(|m| *m == cp) {
        Some(rank) => (rank as u32, 0),
        None => (MARK_ORDER.len() as u32, cp),
    }
}

/// A BYTE-COMPARABLE multi-level ordering key for `s`, in the shape ICU uses
/// and `collation.py`'s `sort_levels` produces.
///
/// Ordering is not the same problem as matching. The single-level fold that
/// `normalize_index_bytes` produces answers "are these equal under this
/// collation"; it cannot answer "which comes first", because two strings
/// differing only in an accent fold to the same bytes and then fall back to
/// comparing whole codepoints -- which puts every accented word after `z`
/// instead of beside its base letter, and drops case order entirely.
///
/// Three levels, compared in order, each terminated by a byte no level body can
/// contain so that a shorter level sorts before a longer one that extends it:
///
/// * **primary** -- base letters: accents removed, case folded. Under
///   `numericOrdering` a digit run is emitted as a fixed-width number so that
///   `a2 < a10`.
/// * **secondary** -- the accents, one group per base character, weighted by
///   `MARK_ORDER`. `backwards` (French) REVERSES this level, which is what
///   makes `cote < côte < coté` rather than `cote < coté < côte`.
/// * **tertiary** -- case, one rank per base character. `caseFirst: "upper"`
///   flips it.
///
/// `strength` truncates: 1 keeps the primary alone, 2 adds the secondary, 3
/// adds the tertiary; `caseLevel` re-adds the case rank at strength 1 and 2.
///
/// This feeds the in-memory SORT only. Index entries are encoded with no
/// collation at all (the one collated `encode_value` call site is the sort-key
/// builder), so nothing here changes bytes already on disk.
pub fn sort_level_bytes(s: &str, c: &Collation) -> Vec<u8> {
    let mut bases: Vec<char> = Vec::new();
    let mut marks: Vec<Vec<(u32, u32)>> = Vec::new();
    let mut cases: Vec<u8> = Vec::new();
    for ch in s.nfd() {
        if is_ccc_mark(ch) {
            if let Some(last) = marks.last_mut() {
                last.push(mark_weight(ch as u32));
            }
            continue;
        }
        bases.push(ch);
        marks.push(Vec::new());
        cases.push(u8::from(ch.is_uppercase()));
    }
    let primary_text = case_fold(&bases.iter().collect::<String>());

    let mut out: Vec<u8> = Vec::with_capacity(primary_text.len() + 8);
    if c.numeric_ordering {
        // Digit runs compare as NUMBERS, text runs as bytes. The tag byte keeps
        // the two kinds from interleaving, and the fixed width makes the number
        // byte-comparable.
        let mut rest = primary_text.as_str();
        while !rest.is_empty() {
            let digits = rest
                .find(|ch: char| !ch.is_ascii_digit())
                .unwrap_or(rest.len());
            if digits > 0 {
                let (run, tail) = rest.split_at(digits);
                out.push(0x01);
                out.extend_from_slice(&run.parse::<u128>().unwrap_or(u128::MAX).to_be_bytes());
                rest = tail;
            } else {
                let end = rest
                    .find(|ch: char| ch.is_ascii_digit())
                    .unwrap_or(rest.len());
                let (run, tail) = rest.split_at(end);
                out.push(0x02);
                push_escaped(&mut out, run.as_bytes());
                rest = tail;
            }
        }
    } else {
        push_escaped(&mut out, primary_text.as_bytes());
    }
    if c.strength <= 1 {
        if c.case_level {
            out.extend_from_slice(&[0x00, 0x00]);
            push_case_ranks(&mut out, &cases, c.case_first_upper);
        }
        return out;
    }
    out.extend_from_slice(&[0x00, 0x00]);
    if c.backwards {
        marks.reverse();
    }
    for group in &marks {
        for (rank, cp) in group {
            out.push(0x01);
            out.extend_from_slice(&rank.to_be_bytes());
            out.extend_from_slice(&cp.to_be_bytes());
        }
        out.push(0x00); // end of this base character's marks
    }
    if c.strength == 2 && !c.case_level {
        return out;
    }
    out.extend_from_slice(&[0x00, 0x00]);
    push_case_ranks(&mut out, &cases, c.case_first_upper);
    out
}

fn push_case_ranks(out: &mut Vec<u8>, cases: &[u8], upper_first: bool) {
    for rank in cases {
        out.push(if upper_first { 1 - *rank } else { *rank } + 1);
    }
}

/// Null-escape so a level body can never contain the `00 00` separator.
fn push_escaped(out: &mut Vec<u8>, bytes: &[u8]) {
    for b in bytes {
        out.push(*b);
        if *b == 0x00 {
            out.push(0xFF);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(strength: i32, case_level: bool) -> Collation {
        Collation {
            strength,
            case_level,
            numeric_ordering: false,
            case_first_upper: false,
            backwards: false,
        }
    }

    #[test]
    fn case_insensitive_ascii() {
        let c = coll(2, false);
        assert_eq!(equal("PING", "ping", &c), Some(true));
        assert_eq!(compare("Apple", "banana", &c), Some(Ordering::Less));
    }

    #[test]
    fn case_sensitive_strength3_identity() {
        let c = coll(3, false);
        assert_eq!(equal("PING", "ping", &c), Some(false));
        // strength 3 has no transform, so even non-ASCII is handled (identity).
        assert_eq!(equal("café", "café", &c), Some(true));
    }

    #[test]
    fn non_ascii_transform_is_handled_not_deferred() {
        // This used to assert `None` ("defer to Python"), which pinned a bug:
        // the Rust server has no Python behind a defer, so every collated
        // comparison touching a non-ASCII character answered `2 BadValue`.
        // mongod 8.2.11 compares these equal under a case-insensitive
        // collation (accents kept at strength 2, case ignored).
        let c = coll(2, false);
        assert_eq!(equal("café", "CAFÉ", &c), Some(true));
        // Strength 2 keeps accents, so these stay distinct.
        assert_eq!(equal("café", "cafe", &c), Some(false));
        // Strength 1 also folds the accent away.
        assert_eq!(equal("café", "CAFE", &coll(1, false)), Some(true));
    }

    #[test]
    fn numeric_ordering_defers() {
        let c = Collation {
            strength: 3,
            case_level: false,
            case_first_upper: false,
            backwards: false,
            numeric_ordering: true,
        };
        assert_eq!(compare("a2", "a10", &c), None);
    }

    #[test]
    fn case_level_keeps_case() {
        // strength 1 + caseLevel: accent-insensitive but case-sensitive.
        let c = coll(1, true);
        assert_eq!(equal("PING", "ping", &c), Some(false));
    }
}
