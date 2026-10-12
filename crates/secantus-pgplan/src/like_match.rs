//! PostgreSQL's own `LIKE` matcher (`like_match.c`'s `MatchText`), for the
//! one shape a regex cannot express: a pattern ending in its escape
//! character.
//!
//! PostgreSQL raises `22025 LIKE pattern must not end with escape character`
//! LAZILY -- only when matching reaches the trailing escape with text left
//! to match. `'b' LIKE 'a\'` is false (the first character already fails)
//! and `relname LIKE 'a\'` over a catalog with no `a...` relation is no rows,
//! which pgjdbc's DatabaseMetaDataTest `escaping()` relies on. Measured on
//! PostgreSQL 15.19.

use crate::{Error, Result};

#[derive(PartialEq)]
enum Outcome {
    True,
    False,
    Abort,
}

fn trailing_escape() -> Error {
    // 22025 invalid_escape_sequence.
    Error::Sqlstate(
        "22025",
        "LIKE pattern must not end with escape character".into(),
    )
}

/// Does `pattern` end in a dangling escape (an escape character with
/// nothing after it to make literal)?
pub(crate) fn ends_in_escape(pattern: &str, escape: Option<char>) -> bool {
    let Some(esc) = escape else {
        return false;
    };
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == esc && chars.next().is_none() {
            return true;
        }
    }
    false
}

/// Does `pattern` hold an unescaped `%` or `_`?
pub(crate) fn has_wildcard(pattern: &str, escape: Option<char>) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if Some(c) == escape {
            chars.next();
        } else if c == '%' || c == '_' {
            return true;
        }
    }
    false
}

/// `subject LIKE pattern` (or ILIKE), PostgreSQL's algorithm step for step,
/// so its trailing-escape error fires exactly when PostgreSQL's does.
pub(crate) fn like_match(
    subject: &str,
    pattern: &str,
    escape: Option<char>,
    insensitive: bool,
) -> Result<bool> {
    let fold = |s: &str| -> Vec<char> {
        if insensitive {
            s.to_lowercase().chars().collect()
        } else {
            s.chars().collect()
        }
    };
    let t = fold(subject);
    let p = fold(pattern);
    let esc = escape.map(|e| {
        if insensitive {
            e.to_lowercase().next().unwrap_or(e)
        } else {
            e
        }
    });
    Ok(match_text(&t, &p, esc)? == Outcome::True)
}

fn match_text(mut t: &[char], mut p: &[char], esc: Option<char>) -> Result<Outcome> {
    if p == ['%'] {
        return Ok(Outcome::True);
    }
    while !t.is_empty() && !p.is_empty() {
        if Some(p[0]) == esc {
            p = &p[1..];
            let Some(&lit) = p.first() else {
                return Err(trailing_escape());
            };
            if lit != t[0] {
                return Ok(Outcome::False);
            }
        } else if p[0] == '%' {
            p = &p[1..];
            while let Some(&c) = p.first() {
                if c == '%' {
                    p = &p[1..];
                } else if c == '_' {
                    if t.is_empty() {
                        return Ok(Outcome::Abort);
                    }
                    t = &t[1..];
                    p = &p[1..];
                } else {
                    break;
                }
            }
            if p.is_empty() {
                return Ok(Outcome::True);
            }
            let first = if Some(p[0]) == esc {
                *p.get(1).ok_or_else(trailing_escape)?
            } else {
                p[0]
            };
            while !t.is_empty() {
                if t[0] == first {
                    let m = match_text(t, p, esc)?;
                    if m != Outcome::False {
                        return Ok(m);
                    }
                }
                t = &t[1..];
            }
            return Ok(Outcome::Abort);
        } else if p[0] == '_' {
            t = &t[1..];
            p = &p[1..];
            continue;
        } else if p[0] != t[0] {
            return Ok(Outcome::False);
        }
        t = &t[1..];
        p = &p[1..];
    }
    if !t.is_empty() {
        return Ok(Outcome::False);
    }
    while p.first() == Some(&'%') {
        p = &p[1..];
    }
    Ok(if p.is_empty() {
        Outcome::True
    } else {
        Outcome::Abort
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_escape_errors_only_when_matching_reaches_it() {
        let e = Some('\\');
        assert!(!like_match("a", "a\\", e, false).unwrap());
        assert!(!like_match("b", "a\\", e, false).unwrap());
        assert!(like_match("ab", "a\\", e, false).is_err());
        assert!(like_match("a\\", "a\\", e, false).is_err());
        assert!(like_match("xab", "%a\\", e, false).is_err());
        assert!(!like_match("xa", "%a\\", e, false).unwrap());
        assert!(like_match("abc", "a%", e, false).unwrap());
        assert!(like_match("ABC", "a_c", e, true).unwrap());
        assert!(like_match("a%", "a\\%", e, false).unwrap());
        assert!(!like_match("ab", "a\\%", e, false).unwrap());
        assert!(ends_in_escape("a\\", e) && !ends_in_escape("a\\\\", e));
    }
}

/// `bytea LIKE bytea`: `%` is any run of bytes, `_` any one byte, and the
/// escape byte takes the next byte literally. A pattern that ends in its
/// escape is 22025, and only when matching reaches it, as for text.
pub(crate) fn like_match_bytes(subject: &[u8], pattern: &[u8], escape: Option<u8>) -> Result<bool> {
    let Some((&first, rest)) = pattern.split_first() else {
        return Ok(subject.is_empty());
    };
    if Some(first) == escape {
        let Some((&literal, rest)) = rest.split_first() else {
            // Reached with nothing left to match, the pattern simply fails.
            if subject.is_empty() {
                return Ok(false);
            }
            return Err(Error::Sqlstate(
                "22025",
                "LIKE pattern must not end with escape character".into(),
            ));
        };
        return match subject.split_first() {
            Some((&b, tail)) if b == literal => like_match_bytes(tail, rest, escape),
            _ => Ok(false),
        };
    }
    match first {
        b'%' => {
            for skip in 0..=subject.len() {
                if like_match_bytes(&subject[skip..], rest, escape)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        b'_' => match subject.split_first() {
            Some((_, tail)) => like_match_bytes(tail, rest, escape),
            None => Ok(false),
        },
        literal => match subject.split_first() {
            Some((&b, tail)) if b == literal => like_match_bytes(tail, rest, escape),
            _ => Ok(false),
        },
    }
}

#[cfg(test)]
mod byte_tests {
    use super::like_match_bytes;

    #[test]
    fn bytes_match_with_both_wildcards_and_an_escape() {
        let m = |s: &[u8], p: &[u8]| like_match_bytes(s, p, Some(b'\\')).unwrap();
        assert!(m(b"abc", b"abc"));
        assert!(m(b"abc", b"a%"));
        assert!(m(b"abc", b"%c"));
        assert!(m(b"abc", b"a_c"));
        assert!(m(b"", b"%"));
        assert!(!m(b"abc", b"a_"));
        assert!(!m(b"abc", b"abd"));
        assert!(m(b"a%c", b"a\\%c"));
        assert!(!m(b"abc", b"a\\%c"));
        assert!(m(&[0, 255, 1], &[0, b'%', 1]));
        // A pattern ending in its escape is an error once matching gets there.
        assert!(like_match_bytes(b"ab", b"a\\", Some(b'\\')).is_err());
        assert!(!like_match_bytes(b"a", b"a\\", Some(b'\\')).unwrap());
        assert!(!like_match_bytes(b"b", b"a\\", Some(b'\\')).unwrap());
    }
}
