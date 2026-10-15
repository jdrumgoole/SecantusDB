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
//!
//! `N'...'` is the same plain literal behind an `NCHAR` keyword (scan.l's
//! `xnstart` eats only the `n`), so it gets the old rules too: it becomes
//! `nchar E'...'`, the tokens the scanner hands the grammar for it.
//!
//! `backslash_quote` is checked by the same walk, whatever the conforming
//! setting: scan.l's `xeescape` refuses a `\'` in a literal read with
//! backslash escapes when it is `off` (or `safe_encoding` under a
//! client-only encoding), as 22P06 at the literal's token (measured on
//! PostgreSQL 15.19: `select E'a\'b'` points at the `E`, `select N'a\'b'`
//! at the quote, since the `n` was a token of its own).

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
    let (out, warnings, _) = rewrite_with(
        sql,
        Syntax {
            conforming: false,
            warn_escapes,
            refuse_backslash_quote: false,
        },
    );
    (out, warnings)
}

/// The session settings the scanner reads a literal by.
#[derive(Debug, Clone, Copy)]
pub struct Syntax {
    /// `standard_conforming_strings`: when on, the text is not rewritten.
    pub conforming: bool,
    /// `escape_string_warning`.
    pub warn_escapes: bool,
    /// `backslash_quote` refuses `\'`: it is `off`, or `safe_encoding`
    /// under a client-only encoding.
    pub refuse_backslash_quote: bool,
}

/// The message and hint of the `backslash_quote` refusal (22P06).
pub const UNSAFE_BACKSLASH_QUOTE: &str = "unsafe use of \\' in a string literal\nHint: Use '' to write quotes in strings. \\' is insecure in client-only encodings.";

/// `rewrite` under `syntax`, plus the 1-based character position of the
/// error when there is one the scanner positions (the `backslash_quote`
/// refusal).
pub fn rewrite_with(
    sql: &str,
    syntax: Syntax,
) -> (Result<String>, Vec<EscapeWarning>, Option<usize>) {
    let mut warnings = Vec::new();
    let mut position = None;
    let out = rewrite_into(sql, syntax, &mut warnings, &mut position);
    (out, warnings, position)
}

fn rewrite_into(
    sql: &str,
    syntax: Syntax,
    warnings: &mut Vec<EscapeWarning>,
    error_position: &mut Option<usize>,
) -> Result<String> {
    let warn_escapes = syntax.warn_escapes && !syntax.conforming;
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
            let (body, keep) = literal_kind(b, i, syntax.conforming);
            if body.is_none() {
                // `U&'...'`: PostgreSQL refuses it outright when the setting
                // is off, rather than guess what the backslashes mean.
                return Err(Error::FeatureNotSupported(
                    "unsafe use of string constant with Unicode escapes\nDetail: String constants with Unicode escapes cannot be used when standard_conforming_strings is off.".into(),
                ));
            }
            let body = body.unwrap_or(Body::Standard);
            // `N'`: the `n` (already copied) is a token of its own, so the
            // literal and its position start at the quote.
            let national = !keep && is_national(b, i);
            let start = if keep { prefix_start(b, i) } else { i };
            let position = sql[..start].chars().count() + 1;
            let mut warned = false;
            if national {
                // `N` + `'...'` is the NCHAR keyword then a plain literal.
                out.pop();
                out.push_str("nchar ");
            }
            if !keep {
                // A plain literal: the parser must read it as `E'...'`. After
                // a word (`int4'1'`, a typed literal) the `E` is a token of
                // its own, or it would join the type name (`int4E`).
                if !national && prev_is_ident(b, i) {
                    out.push(' ');
                }
                out.push('E');
            }
            let mut segment = Segment {
                warn: !keep && warn_escapes,
                refuse: syntax.refuse_backslash_quote && body == Body::Escaped,
                position,
                warned: &mut warned,
                warnings: &mut *warnings,
                error_position: &mut *error_position,
            };
            segment.scan(b, i + 1)?;
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
                None => return Err(segment.fail(unterminated())),
            };
            // `'a'` <newline> `'b'` continues the same literal in the same
            // state, so the continuation of a plain literal needs the escaped
            // body rule too (and, being one literal, no second `E`).
            while let Some(next) = continuation_after(b, end) {
                segment.scan(b, next + 1)?;
                end = match skip_literal(b, next + 1, body) {
                    Some(end) => end,
                    None if keep => b.len(),
                    None => return Err(segment.fail(unterminated())),
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

/// What the scanner raises over one literal (or one continuation of it):
/// the `escape_string_warning` for the literal's first escape, and the
/// `backslash_quote` refusal. `xeescape` refuses a `\\'` BEFORE it warns,
/// so a literal whose first escape is the refused one only errors.
struct Segment<'a> {
    warn: bool,
    refuse: bool,
    position: usize,
    warned: &'a mut bool,
    warnings: &'a mut Vec<EscapeWarning>,
    error_position: &'a mut Option<usize>,
}

impl Segment<'_> {
    fn scan(&mut self, b: &[u8], from: usize) -> Result<()> {
        let refused = self.refuse && has_backslash_quote(b, from);
        if self.warn && !*self.warned {
            if let Some(w) = first_escape_warning(b, from, self.position) {
                if refused && w.message.contains("\\'") {
                    return Err(self.refusal());
                }
                self.warnings.push(w);
                *self.warned = true;
            }
        }
        if refused {
            return Err(self.refusal());
        }
        Ok(())
    }

    fn refusal(&mut self) -> Error {
        self.fail(Error::Sqlstate(SQLSTATE, UNSAFE_BACKSLASH_QUOTE.into()))
    }

    /// `e`, raised at the literal's position.
    fn fail(&mut self, e: Error) -> Error {
        *self.error_position = Some(self.position);
        e
    }
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
/// When `conforming`, nothing is rewritten: a plain literal is kept with
/// the standard body and `U&'` is the parser's own.
fn literal_kind(b: &[u8], i: usize, conforming: bool) -> (Option<Body>, bool) {
    // `E'`, `B'`, `X'` when the letter is a token of its own.
    if i >= 1 && !prev_is_ident(b, i - 1) {
        match b[i - 1] {
            b'e' | b'E' => return (Some(Body::Escaped), true),
            b'b' | b'B' | b'x' | b'X' => return (Some(Body::Standard), true),
            _ => {}
        }
    }
    if conforming {
        return (Some(Body::Standard), true);
    }
    if i >= 2 && b[i - 1] == b'&' && matches!(b[i - 2], b'u' | b'U') && !prev_is_ident(b, i - 2) {
        return (None, true);
    }
    (Some(Body::Escaped), false)
}

/// Is the quote at `i` the start of `N'...'` (an `n` that is a token of
/// its own)?
fn is_national(b: &[u8], i: usize) -> bool {
    i >= 1 && matches!(b[i - 1], b'n' | b'N') && !prev_is_ident(b, i - 1)
}

/// Where a kept literal's token starts: at its one-letter prefix, or at
/// the `U&`.
fn prefix_start(b: &[u8], i: usize) -> usize {
    if i >= 2 && b[i - 1] == b'&' && matches!(b[i - 2], b'u' | b'U') && !prev_is_ident(b, i - 2) {
        i - 2
    } else if i >= 1
        && matches!(b[i - 1], b'e' | b'E' | b'b' | b'B' | b'x' | b'X')
        && !prev_is_ident(b, i - 1)
    {
        i - 1
    } else {
        i
    }
}

/// Does the escaped literal body starting at `from` hold a `\'` before it
/// ends? (`xeescape` matches a backslash and the character after it.)
fn has_backslash_quote(b: &[u8], from: usize) -> bool {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                if b.get(i + 1) == Some(&b'\'') {
                    return true;
                }
                i += 2;
            }
            b'\'' => {
                if b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                } else {
                    return false;
                }
            }
            _ => i += 1,
        }
    }
    false
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

    fn syntax(conforming: bool, refuse: bool) -> Syntax {
        Syntax {
            conforming,
            warn_escapes: true,
            refuse_backslash_quote: refuse,
        }
    }

    #[test]
    fn national_literals_get_the_old_rules_behind_nchar() {
        assert_eq!(
            rw(r"select N'a\\b', n'x'"),
            (
                r"select nchar E'a\\b', nchar E'x'".into(),
                vec![r"nonstandard use of \\ in a string literal@9".to_string()]
            )
        );
    }

    #[test]
    fn backslash_quote_refuses_where_backslashes_escape() {
        let refused = |sql: &str, conforming: bool| {
            let (out, w, pos) = rewrite_with(sql, syntax(conforming, true));
            (
                matches!(out, Err(Error::Sqlstate("22P06", m)) if m.starts_with("unsafe use of")),
                w.len(),
                pos,
            )
        };
        // Its first escape is the refused one: no warning, only the error.
        assert_eq!(refused(r"select 'a\'b'", false), (true, 0, Some(8)));
        // An earlier escape warns first.
        assert_eq!(refused(r"select 'a\\b\'c'", false), (true, 1, Some(8)));
        assert_eq!(refused(r"select 1, E'a\'b'", false), (true, 0, Some(11)));
        assert_eq!(refused(r"select N'a\'b'", false), (true, 0, Some(9)));
        assert_eq!(refused(r"select E'a\'b'", true), (true, 0, Some(8)));
        // Conforming, a plain literal has no escapes; nor has a dollar body.
        assert_eq!(refused(r"select 'a\', $$\'$$", true), (false, 0, None));
        assert_eq!(refused(r"select 'a\\''b'", false), (false, 1, None));
        assert_eq!(
            rewrite_with(r"select 'a\'b'", syntax(false, false)).0,
            Ok(r"select E'a\'b'".into())
        );
    }

    #[test]
    fn a_typed_literal_keeps_its_type_name() {
        assert_eq!(
            rw(r"select int4'1', date'2020-01-02', x'1f', n'a', (1)||'b'"),
            (
                r"select int4 E'1', date E'2020-01-02', x'1f', nchar E'a', (1)||E'b'".into(),
                vec![]
            )
        );
    }
}
