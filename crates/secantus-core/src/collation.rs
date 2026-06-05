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

use bson::{Bson, Document};

pub struct Collation {
    pub strength: i32,
    pub case_level: bool,
    pub numeric_ordering: bool,
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
    })
}

/// Normalise a string under the collation, or `None` if the case can't be
/// reproduced version-independently (non-ASCII transform, or numericOrdering).
fn normalize(s: &str, c: &Collation) -> Option<String> {
    if c.numeric_ordering {
        return None; // needs Python's digit-run tuple ordering
    }
    let accent = c.accent_insensitive();
    let case = c.case_insensitive();
    if !accent && !case {
        return Some(s.to_string()); // no transform -> identity, any string
    }
    if !s.is_ascii() {
        return None; // accent/case transform on non-ASCII -> defer to Python
    }
    // ASCII: accent stripping is a no-op; casefold == ASCII lowercasing.
    Some(if case {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    })
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
    if !s.is_ascii() {
        return None; // accent/case transform on non-ASCII -> defer
    }
    Some(if case {
        s.to_ascii_lowercase().into_bytes()
    } else {
        s.as_bytes().to_vec()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(strength: i32, case_level: bool) -> Collation {
        Collation {
            strength,
            case_level,
            numeric_ordering: false,
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
    fn non_ascii_transform_defers() {
        let c = coll(2, false); // case-insensitive
        assert_eq!(equal("café", "CAFÉ", &c), None); // -> Python
    }

    #[test]
    fn numeric_ordering_defers() {
        let c = Collation {
            strength: 3,
            case_level: false,
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
