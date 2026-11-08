//! `LIKE` / `ILIKE` / `SIMILAR TO` and the POSIX operators `~` `~*` `!~` `!~*`.
//!
//! Every pattern is rewritten to a regular expression once, so the same
//! source drives a constant fold (`select 'abc' like 'a%'`), a select-list
//! expression over a row, and a WHERE clause lowered to MQL's `$regex`. The
//! rewrites follow PostgreSQL's own (`like_match.c`, `similar_escape` in
//! `regexp.c`): `%` is `.*`, `_` is one character, the escape character
//! (backslash unless `ESCAPE` says otherwise) makes the next character
//! literal, and `.` matches a newline — a LIKE `%` spans lines, and so does an
//! ARE `.` (measured PostgreSQL 16.15).

use crate::{Error, Result};

/// The behaviour of one operator in the family, as the parser names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    /// `LIKE` (`~~`), `ILIKE` (`~~*`), `SIMILAR TO` (`~`) or a bare POSIX
    /// regex (`~`, `~*`).
    pub kind: Kind,
    pub negated: bool,
    pub case_insensitive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Like,
    Similar,
    Posix,
}

impl Op {
    /// The operator an `A_Expr` names: `~~` / `!~~` / `~~*` / `!~~*` are
    /// LIKE and its variants (the parser attaches these to a `LIKE` node and
    /// accepts them spelled out too); `~` / `!~` / `~*` / `!~*` are the POSIX
    /// operators -- or `SIMILAR TO` when the node's kind says so, since the
    /// parser reuses the `~` name for it.
    pub fn from_operator(name: &str, similar: bool) -> Option<Op> {
        let negated = name.starts_with('!');
        let case_insensitive = name.ends_with('*');
        let bare = name.trim_start_matches('!').trim_end_matches('*');
        let kind = match (bare, similar) {
            ("~~", false) => Kind::Like,
            ("~", true) => Kind::Similar,
            ("~", false) => Kind::Posix,
            _ => return None,
        };
        Some(Op {
            kind,
            negated,
            case_insensitive,
        })
    }

    /// The operator's spelling, for an error message.
    pub fn name(self) -> &'static str {
        match (self.kind, self.negated, self.case_insensitive) {
            (Kind::Like, false, false) => "~~",
            (Kind::Like, true, false) => "!~~",
            (Kind::Like, false, true) => "~~*",
            (Kind::Like, true, true) => "!~~*",
            (_, false, false) => "~",
            (_, true, false) => "!~",
            (_, false, true) => "~*",
            (_, true, true) => "!~*",
        }
    }

    /// The regex source for `pattern`, without flags: what a `$regex` carries.
    pub fn regex_source(self, pattern: &str, escape: Option<&str>) -> Result<String> {
        match self.kind {
            Kind::Like => like_to_regex(pattern, escape),
            Kind::Similar => similar_to_regex(pattern, escape),
            Kind::Posix => Ok(pattern.to_string()),
        }
    }

    /// The `$options` string MQL needs: `.` spans lines always, and `i` when
    /// the operator folds case.
    pub fn regex_options(self) -> &'static str {
        if self.case_insensitive {
            "si"
        } else {
            "s"
        }
    }

    /// Whether `text` matches `pattern` -- BEFORE negation, which the caller
    /// applies so a NULL operand can stay NULL.
    pub fn matches(self, text: &str, pattern: &str, escape: Option<&str>) -> Result<bool> {
        let source = self.regex_source(pattern, escape)?;
        let re = compile(&source, self.case_insensitive, pattern)?;
        Ok(re.is_match(text))
    }
}

/// The compiled form: `(?s)` so `.` spans newlines, `(?i)` when folding case.
pub fn compile(source: &str, case_insensitive: bool, _shown: &str) -> Result<regex::Regex> {
    let flags = if case_insensitive { "(?si)" } else { "(?s)" };
    regex::Regex::new(&format!("{flags}{source}")).map_err(|e| {
        Error::InvalidRegex(format!("invalid regular expression: {}", regex_fault(&e)))
    })
}

/// PostgreSQL's wording for the syntax faults the `regex` crate reports;
/// the crate's own text (with its caret diagram) for anything else.
fn regex_fault(e: &regex::Error) -> String {
    let text = e.to_string();
    let last = text
        .lines()
        .last()
        .unwrap_or("")
        .trim_start_matches("error: ");
    let mapped = if last.contains("unclosed group") || last.contains("unopened group") {
        "parentheses () not balanced"
    } else if last.contains("unclosed character class") {
        "brackets [] not balanced"
    } else if last.contains("repetition operator missing expression") {
        "quantifier operand invalid"
    } else if last.contains("repetition") {
        "invalid repetition count(s)"
    } else if last.contains("escape") {
        "invalid escape \\ sequence"
    } else {
        last
    };
    mapped.to_string()
}

/// The escape character an `ESCAPE` clause names: one character, or none
/// when the clause is the empty string (which switches escaping off).
fn escape_char(escape: Option<&str>) -> Result<Option<char>> {
    match escape {
        None => Ok(Some('\\')),
        Some(e) => {
            let mut chars = e.chars();
            match (chars.next(), chars.next()) {
                (None, _) => Ok(None),
                (Some(c), None) => Ok(Some(c)),
                _ => Err(Error::InvalidEscapeSequence(
                    "invalid escape string\nHint: Escape string must be empty or one character."
                        .into(),
                )),
            }
        }
    }
}

/// `LIKE` pattern -> anchored regex: `%` -> `.*`, `_` -> `.`, everything else
/// literal. A pattern ending in the escape character is PostgreSQL's 22025.
pub fn like_to_regex(pattern: &str, escape: Option<&str>) -> Result<String> {
    let esc = escape_char(escape)?;
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if Some(c) == esc {
            match chars.next() {
                Some(lit) => out.push_str(&regex::escape(&lit.to_string())),
                None => {
                    return Err(Error::InvalidEscapeSequence(
                        "LIKE pattern must not end with escape character".into(),
                    ))
                }
            }
        } else if c == '%' {
            // Runs of `%` are one `.*`; `%_` needs the `_` to consume a char.
            while chars.peek() == Some(&'%') {
                chars.next();
            }
            out.push_str(".*");
        } else if c == '_' {
            out.push('.');
        } else {
            out.push_str(&regex::escape(&c.to_string()));
        }
    }
    out.push('$');
    Ok(out)
}

/// `SIMILAR TO` pattern -> anchored regex, PostgreSQL's `similar_escape`
/// rewrite: `%` / `_` as in LIKE, `|` `*` `+` `?` `{}` `[]` kept as regex,
/// `(` made non-capturing, and `. ^ $ \` made literal (the period is NOT a
/// metacharacter for SIMILAR TO).
pub fn similar_to_regex(pattern: &str, escape: Option<&str>) -> Result<String> {
    let esc = escape_char(escape)?;
    let mut out = String::from("^(?:");
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if Some(c) == esc && !in_class {
            match chars.next() {
                Some(lit) => out.push_str(&regex::escape(&lit.to_string())),
                None => {
                    return Err(Error::InvalidEscapeSequence(
                        "LIKE pattern must not end with escape character".into(),
                    ))
                }
            }
        } else if in_class {
            if c == '\\' {
                out.push('\\');
            }
            out.push(c);
            if c == ']' {
                in_class = false;
            }
        } else {
            match c {
                '[' => {
                    out.push('[');
                    in_class = true;
                }
                '%' => out.push_str(".*"),
                '_' => out.push('.'),
                '(' => out.push_str("(?:"),
                '\\' | '.' | '^' | '$' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
    }
    out.push_str(")$");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn like(text: &str, pat: &str) -> bool {
        Op::from_operator("~~", false)
            .unwrap()
            .matches(text, pat, None)
            .unwrap()
    }
    fn similar(text: &str, pat: &str) -> bool {
        Op::from_operator("~", true)
            .unwrap()
            .matches(text, pat, None)
            .unwrap()
    }

    #[test]
    fn like_wildcards_and_escapes() {
        assert!(like("abc", "a%"));
        assert!(like("abc", "a_c"));
        assert!(!like("abc", "a_"));
        assert!(like("a_c", "a\\_c"));
        assert!(!like("abc", "a\\_c"));
        assert!(like("a%", "a\\%"));
        assert!(like("a\nb", "a%b"));
        assert!(like("", "%"));
        assert!(!like("", "_"));
        assert!(like("ä", "_"));
        assert!(like("a.c", "a.c"));
        assert!(!like("abc", "a.c"));
        assert!(!like("A", "a"));
        assert!(Op::from_operator("~~*", false)
            .unwrap()
            .matches("A", "a", None)
            .unwrap());
        let esc = Op::from_operator("~~", false).unwrap();
        assert!(esc.matches("a_", "aX_", Some("X")).unwrap());
        assert!(!esc.matches("aXb", "aX_", Some("X")).unwrap());
        assert!(esc.matches("a\\", "a\\", Some("")).unwrap());
        assert_eq!(
            esc.matches("a", "a\\", None).unwrap_err().sqlstate(),
            "22025"
        );
        assert_eq!(
            esc.matches("a", "a", Some("xx")).unwrap_err().sqlstate(),
            "22025"
        );
    }

    #[test]
    fn similar_to_is_a_regex_with_like_wildcards() {
        assert!(similar("abc", "a%"));
        assert!(similar("ab", "(a|c)b"));
        assert!(similar("ab", "a[b]"));
        assert!(similar("5", "[0-9]"));
        assert!(!similar("ab", "a."));
        assert!(similar("a.", "a."));
        assert!(!similar("abc", "a{2}"));
        assert!(similar("aab", "a{2}b"));
        assert_eq!(
            Op::from_operator("~", true)
                .unwrap()
                .matches("ab", "a(b", None)
                .unwrap_err()
                .sqlstate(),
            "2201B"
        );
    }

    #[test]
    fn posix_operators() {
        let re = Op::from_operator("~", false).unwrap();
        assert!(re.matches("abc", "^a", None).unwrap());
        assert!(!re.matches("Ab", "ab", None).unwrap());
        assert!(Op::from_operator("~*", false)
            .unwrap()
            .matches("Ab", "ab", None)
            .unwrap());
        assert!(re.matches("a\nb", "a.b", None).unwrap());
        assert!(!re.matches("a\nb", "^b", None).unwrap());
        assert!(Op::from_operator("!~", false).unwrap().negated);
        assert_eq!(re.matches("a", "(", None).unwrap_err().sqlstate(), "2201B");
        for (pat, msg) in [
            ("(", "parentheses () not balanced"),
            (")", "parentheses () not balanced"),
            ("[", "brackets [] not balanced"),
            ("*a", "quantifier operand invalid"),
            ("a{2,1}", "invalid repetition count(s)"),
            ("\\", "invalid escape \\ sequence"),
        ] {
            assert_eq!(
                re.matches("a", pat, None).unwrap_err().to_string(),
                format!("invalid regular expression: {msg}"),
                "{pat}"
            );
        }
    }
}
