//! `standard_conforming_strings = off`: the pre-2009 string-literal syntax.
//!
//! libpg_query parses with the setting hard-wired ON, so a session that turns
//! it off gets its statement text rewritten before parsing: every plain
//! `'...'` literal becomes the `E'...'` literal it means under the old rules,
//! which the parser then reads with exactly the backslash semantics
//! PostgreSQL's scanner applies to a plain literal when the setting is off
//! (scan.l enters the same `xe` state for both).
//!
//! The rewrite is a lexer, not a parser: it has to know where a plain literal
//! is (not inside a comment, a quoted identifier, a dollar-quoted body, or an
//! `E'` / `B'` / `X'` / `U&'` literal that already has its own rules) and how
//! far it runs (under the old rules `\'` does not end it). Everything else is
//! copied through untouched.
//!
//! The scanner's `escape_string_warning` notices ride along: one `22P06`
//! WARNING per literal, for the FIRST escape in it, worded by that escape
//! (measured on PostgreSQL 16).

use crate::{Error, Result};

/// One `nonstandard use of ... in a string literal` WARNING (SQLSTATE 22P06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeWarning {
    pub message: String,
    pub hint: String,
    /// 1-based character position of the literal, the `P` field.
    pub position: usize,
}

pub const SQLSTATE: &str = "22P06";

/// What the scanner is inside, after a literal closes: a continuation
/// (`'a'` newline `'b'`) keeps the state of the literal it continues.
#[derive(Clone, Copy, PartialEq)]
enum Body {
    /// `''` is the only escape.
    Standard,
    /// Backslash escapes the next character, `''` still works.
    Escaped,
}

/// Rewrite `sql` as the parser reads it with `standard_conforming_strings`
/// on. `warn_escapes` is the session's `escape_string_warning`. The warnings
/// come back beside the result rather than inside it because the scanner
/// raises them as it goes: the ones before an unterminated literal are sent
/// before its error.
pub fn rewrite(sql: &str, warn_escapes: bool) -> (Result<String>, Vec<EscapeWarning>) {
    let mut warnings = Vec::new();
    let out = rewrite_into(sql, warn_escapes, &mut warnings);
    (out, warnings)
}

fn rewrite_into(
    sql: &str,
    warn_escapes: bool,
    warnings: &mut Vec<EscapeWarning>,
) -> Result<String> {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // `-- ...` to end of line.
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            let end = memchr_from(b, i, b'\n').unwrap_or(b.len());
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        // `/* ... */`, nesting as PostgreSQL's does.
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = skip_block_comment(b, i);
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        // A quoted identifier: `""` doubles, backslash means nothing.
        if c == b'"' {
            let end = skip_quoted(b, i + 1, b'"');
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        // A dollar-quoted body, when the `$` starts a token.
        if c == b'$' && !prev_is_ident(b, i) {
            if let Some(tag_end) = dollar_tag_end(b, i) {
                let tag = &b[i..tag_end];
                let end = find_bytes(b, tag_end, tag).map_or(b.len(), |p| p + tag.len());
                out.push_str(&sql[i..end]);
                i = end;
                continue;
            }
        }
        if c == b'\'' {
            let (body, keep) = literal_kind(b, i);
            if body.is_none() {
                // `U&'...'`: PostgreSQL refuses it outright when the setting
                // is off, rather than guess what the backslashes mean.
                return Err(Error::FeatureNotSupported(
                    "unsafe use of string constant with Unicode escapes\nDetail: String constants with Unicode escapes cannot be used when standard_conforming_strings is off.".into(),
                ));
            }
            let body = body.unwrap_or(Body::Standard);
            let position = sql[..i].chars().count() + 1;
            let mut warned = false;
            if !keep {
                // A plain literal: the parser must read it as `E'...'`.
                out.push('E');
                if warn_escapes {
                    if let Some(w) = first_escape_warning(b, i + 1, position) {
                        warnings.push(w);
                        warned = true;
                    }
                }
            }
            // An unterminated PLAIN literal is reported here, naming the text
            // the client wrote: the parser would name the rewritten `E'...'`.
            let unterminated = || {
                let rest = &sql[i..];
                let rest = rest.split_once('\n').map_or(rest, |(line, _)| line);
                Error::Parse(format!("unterminated quoted string at or near \"{rest}\""))
            };
            let mut end = match skip_literal(b, i + 1, body) {
                Some(end) => end,
                None if keep => b.len(),
                None => return Err(unterminated()),
            };
            // `'a'` <newline> `'b'` continues the same literal in the same
            // state, so the continuation of a plain literal needs the escaped
            // body rule too (and, being one literal, no second `E`).
            while let Some(next) = continuation_after(b, end) {
                if !keep && warn_escapes && !warned {
                    if let Some(w) = first_escape_warning(b, next + 1, position) {
                        warnings.push(w);
                        warned = true;
                    }
                }
                end = match skip_literal(b, next + 1, body) {
                    Some(end) => end,
                    None if keep => b.len(),
                    None => return Err(unterminated()),
                };
            }
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        // Copy one whole character.
        let len = utf8_len(c);
        out.push_str(&sql[i..(i + len).min(b.len())]);
        i += len;
    }
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && is_ident(b[i - 1])
}

fn memchr_from(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    b[from..]
        .iter()
        .position(|&c| c == needle)
        .map(|p| p + from)
}

fn find_bytes(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn skip_block_comment(b: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// Past the closing `q` of a quoted run starting at `from` (just inside the
/// opening quote), where `qq` is a doubled quote. Returns `b.len()` when
/// unterminated.
fn skip_quoted(b: &[u8], from: usize, q: u8) -> usize {
    let mut i = from;
    while i < b.len() {
        if b[i] == q {
            if b.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// `$tag$` at `i`: the index past its closing `$`, when `$` opens one.
fn dollar_tag_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(j) == Some(&b'$') {
        return Some(j + 1);
    }
    let first = *b.get(j)?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first >= 0x80) {
        return None;
    }
    while j < b.len() && (is_ident(b[j]) && b[j] != b'$') {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then_some(j + 1)
}

/// The rule for the literal whose opening quote is at `i`, and whether it
/// already carries a prefix the parser reads itself (`keep`). `None` for a
/// `U&'` literal, which the old syntax cannot carry.
fn literal_kind(b: &[u8], i: usize) -> (Option<Body>, bool) {
    // `E'`, `B'`, `X'` when the letter is a token of its own.
    if i >= 1 && !prev_is_ident(b, i - 1) {
        match b[i - 1] {
            b'e' | b'E' => return (Some(Body::Escaped), true),
            b'b' | b'B' | b'x' | b'X' => return (Some(Body::Standard), true),
            _ => {}
        }
    }
    if i >= 2 && b[i - 1] == b'&' && matches!(b[i - 2], b'u' | b'U') && !prev_is_ident(b, i - 2) {
        return (None, true);
    }
    (Some(Body::Escaped), false)
}

/// Past the closing quote of a literal body starting at `from`; `None` when
/// the text ends first.
fn skip_literal(b: &[u8], from: usize, body: Body) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' if body == Body::Escaped => i += 2,
            b'\'' => {
                if b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                } else {
                    return Some(i + 1);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// The opening quote of a continuation literal after `end`: whitespace that
/// includes a newline, then `'`.
fn continuation_after(b: &[u8], end: usize) -> Option<usize> {
    let mut i = end;
    let mut newline = false;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
        newline |= b[i] == b'\n' || b[i] == b'\r';
        i += 1;
    }
    (newline && b.get(i) == Some(&b'\'')).then_some(i)
}

/// The WARNING for the first backslash in the literal body at `from`, if any.
fn first_escape_warning(b: &[u8], from: usize, position: usize) -> Option<EscapeWarning> {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                let (message, hint) = match b.get(i + 1) {
                    Some(b'\'') => (
                        "nonstandard use of \\' in a string literal",
                        "Use '' to write quotes in strings, or use the escape string syntax (E'...').",
                    ),
                    Some(b'\\') => (
                        "nonstandard use of \\\\ in a string literal",
                        "Use the escape string syntax for backslashes, e.g., E'\\\\'.",
                    ),
                    _ => (
                        "nonstandard use of escape in a string literal",
                        "Use the escape string syntax for escapes, e.g., E'\\r\\n'.",
                    ),
                };
                return Some(EscapeWarning {
                    message: message.into(),
                    hint: hint.into(),
                    position,
                });
            }
            b'\'' => {
                if b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                return None;
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw(sql: &str) -> (String, Vec<String>) {
        let (out, warnings) = rewrite(sql, true);
        (
            out.unwrap(),
            warnings
                .iter()
                .map(|w| format!("{}@{}", w.message, w.position))
                .collect(),
        )
    }

    #[test]
    fn plain_literals_become_escape_literals() {
        assert_eq!(
            rw(r"select 'a\'b', 'x\\y', 'p\nq', 'c''d', E'\\', B'01', X'1f', $$a\b'$$, $q$'\'$q$"),
            (
                r"select E'a\'b', E'x\\y', E'p\nq', E'c''d', E'\\', B'01', X'1f', $$a\b'$$, $q$'\'$q$".into(),
                vec![
                    r"nonstandard use of \' in a string literal@8".to_string(),
                    r"nonstandard use of \\ in a string literal@16".to_string(),
                    "nonstandard use of escape in a string literal@24".to_string(),
                ]
            )
        );
    }

    #[test]
    fn comments_identifiers_and_dollar_bodies_are_untouched() {
        let sql = "select 'a' /* '\\' */ , \"c'\\\", 1 -- 'x\\' \n from (select 1 as \"c'\\\") s";
        assert_eq!(rw(sql), (sql.replacen("'a'", "E'a'", 1), vec![]));
        assert_eq!(
            rw("select $1, x$y, 'q'"),
            ("select $1, x$y, E'q'".into(), vec![])
        );
    }

    #[test]
    fn a_continuation_stays_one_literal() {
        assert_eq!(
            rw("select 'a\\'b'\n'\\\\'"),
            (
                "select E'a\\'b'\n'\\\\'".into(),
                vec![r"nonstandard use of \' in a string literal@8".to_string()]
            )
        );
        // The continuation's body follows the escaped rule too: its `\'`
        // does not close it.
        assert_eq!(
            rw("select 'a'\n'b\\'c', 'd'"),
            (
                "select E'a'\n'b\\'c', E'd'".into(),
                vec![r"nonstandard use of \' in a string literal@8".to_string()]
            )
        );
    }

    #[test]
    fn warnings_are_optional_and_unicode_literals_refused() {
        assert_eq!(rewrite(r"select 'a\nb'", false).1, vec![]);
        assert!(matches!(
            rewrite("select U&'d\\0061t'", true).0,
            Err(Error::FeatureNotSupported(m)) if m.starts_with("unsafe use of string constant")
        ));
        // The warning the scanner raised before running out of text is kept.
        let (out, w) = rewrite("select $$a\\b'$$, 'q\\'", true);
        assert_eq!(
            out,
            Err(Error::Parse(
                "unterminated quoted string at or near \"'q\\'\"".into()
            ))
        );
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].position, 18);
        // Positions count characters, not bytes.
        let (_, w) = rewrite("select '\u{20ac}', 'a\\nb'", true);
        assert_eq!(w[0].position, 13);
    }
}
