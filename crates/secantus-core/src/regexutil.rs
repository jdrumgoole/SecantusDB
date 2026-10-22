//! Shared regex compilation + matching, used by both `query::$regex` and the
//! `$regexMatch` / `$regexFind` / `$regexFindAll` aggregation expressions.
//!
//! Mirrors `secantus.query` / `secantus.expressions` regex handling: the linear
//! `regex` crate is the fast path for almost every pattern; a pattern it can't
//! compile (lookaround / backreferences) falls back to the backtracking
//! `fancy-regex`. Only a non-string pattern/options, a pattern over the length
//! cap, or one neither engine compiles signals defer (`Err(())`).
//!
//! `is_match` (Python `re.search` truthiness) and positional find
//! (`find_first` / `find_all`, backing `$regexFind` / `$regexFindAll`) work on
//! both engines; a backtracking engine's captures are converted by
//! `to_match_fancy`, and only its backtrack-limit error defers.

use bson::Bson;
use regex::{Regex as LinearRegex, RegexBuilder};

/// The longest pattern mongod compiles, in UTF-8 BYTES: mongod 8.2.11 sets
/// PCRE2's maximum pattern length, so 32,764 bytes compile and 32,765 are
/// `51091 Regular expression is invalid: pattern string is longer than the
/// limit set by the application` (measured 2026-10-16, with ASCII and with
/// two-byte characters, and with every `$options` flag). The 1,000 that stood
/// here was a ReDoS cap for Python's backtracking `re`, not mongod's limit.
pub(crate) const MAX_REGEX_PATTERN_LEN: usize = 32_764;

/// PCRE2's message for a pattern over [`MAX_REGEX_PATTERN_LEN`].
pub(crate) const PATTERN_TOO_LONG: &str =
    "pattern string is longer than the limit set by the application";

/// A compiled regex from whichever engine could build it. Both run unanchored
/// (`re.search`) semantics.
pub(crate) enum CompiledRegex {
    /// A pattern with no metacharacters, searched as a substring. The linear
    /// engine's lazy DFA gives up on a long pattern and its PikeVM fallback
    /// is O(pattern x subject): a 32,764-character literal over a 40,000-
    /// character string took 70 s in a debug build. A substring search is
    /// linear, and a literal matches the same text either way.
    Literal(String),
    Linear(LinearRegex),
    Fancy(fancy_regex::Regex),
}

/// One `re.search` / `finditer` hit: the matched text, its start as a
/// *code-point* index (Python `m.start()` — not a byte offset), and the capture
/// groups (each the matched substring, or `None` for a non-participating group).
pub(crate) struct RegexMatch {
    pub text: String,
    pub codepoint_idx: usize,
    pub captures: Vec<Option<String>>,
}

impl CompiledRegex {
    pub(crate) fn is_match(&self, s: &str) -> bool {
        match self {
            CompiledRegex::Literal(lit) => s.contains(lit.as_str()),
            CompiledRegex::Linear(re) => re.is_match(s),
            // fancy-regex's is_match is fallible (e.g. backtrack-limit hit); a
            // failure is treated as no-match to stay sound (never over-match).
            CompiledRegex::Fancy(re) => re.is_match(s).unwrap_or(false),
        }
    }

    /// First match (Python `re.search`), or `None`. `Err(())` → defer (a
    /// backtrack-limit error from the fancy engine).
    pub(crate) fn find_first(&self, s: &str) -> Result<Option<RegexMatch>, ()> {
        match self {
            CompiledRegex::Literal(lit) => {
                Ok(s.find(lit.as_str()).map(|at| literal_match(s, lit, at)))
            }
            CompiledRegex::Linear(re) => Ok(re.captures(s).map(|c| to_match(s, &c))),
            CompiledRegex::Fancy(re) => match re.captures(s) {
                Ok(Some(c)) => Ok(Some(to_match_fancy(s, &c))),
                Ok(None) => Ok(None),
                Err(_) => Err(()), // backtrack limit / engine error -> defer
            },
        }
    }

    /// Every match left to right, as mongod's `$regexFindAll` scans: the next
    /// search starts where a match ended, or one character on after an empty
    /// one, and the scan stops once that start reaches the end of the string.
    /// So an empty-matching pattern finds one match per character and none at
    /// the end ("abc" with "" is 3), an empty string still gets its one
    /// match, and an empty match straight after a non-empty one counts (`a*`
    /// over "abc" is "a", "", ""; measured 8.2.11, 2026-10-22). The engines'
    /// own iterators differ at both ends. `Err(())` → defer (a backtrack-limit
    /// error from the fancy engine).
    pub(crate) fn find_all(&self, s: &str) -> Result<Vec<RegexMatch>, ()> {
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let found = match self {
                CompiledRegex::Literal(lit) => s[pos..].find(lit.as_str()).map(|at| {
                    let at = pos + at;
                    (at, at + lit.len(), literal_match(s, lit, at))
                }),
                CompiledRegex::Linear(re) => re.captures_at(s, pos).map(|c| {
                    let whole = c.get(0).unwrap();
                    (whole.start(), whole.end(), to_match(s, &c))
                }),
                CompiledRegex::Fancy(re) => match re.captures_from_pos(s, pos) {
                    Ok(Some(c)) => {
                        let whole = c.get(0).unwrap();
                        Some((whole.start(), whole.end(), to_match_fancy(s, &c)))
                    }
                    Ok(None) => None,
                    Err(_) => return Err(()),
                },
            };
            let Some((start, end, m)) = found else {
                break;
            };
            out.push(m);
            pos = if end > start {
                end
            } else {
                start + s[start..].chars().next().map_or(1, char::len_utf8)
            };
            if pos >= s.len() {
                break;
            }
        }
        Ok(out)
    }
}

/// A [`CompiledRegex::Literal`] hit at byte offset `at`: no capture groups.
fn literal_match(s: &str, lit: &str, at: usize) -> RegexMatch {
    RegexMatch {
        text: lit.to_string(),
        codepoint_idx: s[..at].chars().count(),
        captures: Vec::new(),
    }
}

/// Whether `pat`, under no flag that changes how a literal matches, is a plain
/// string: non-empty and free of every character PCRE gives a meaning to.
fn plain_literal(pat: &str, ci: bool, ext: bool) -> bool {
    !ci && !ext && !pat.is_empty() && !pat.chars().any(|c| "\\.+*?()|[]{}^$".contains(c))
}

/// Build one `RegexMatch` from a linear-engine capture set. Group 0 is the whole
/// match; groups `1..` are `Some(text)` / `None` exactly like Python `m.groups()`.
fn to_match(s: &str, caps: &regex::Captures) -> RegexMatch {
    let whole = caps.get(0).unwrap();
    let captures = (1..caps.len())
        .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
        .collect();
    RegexMatch {
        text: whole.as_str().to_string(),
        codepoint_idx: s[..whole.start()].chars().count(),
        captures,
    }
}

/// Same as [`to_match`] for a fancy-engine capture set. The backtracking engine
/// is Perl/Python-`re`-compatible, so its leftmost-first match and per-group
/// participation line up with Python's for the lookaround / backreference
/// patterns that reach this path.
fn to_match_fancy(s: &str, caps: &fancy_regex::Captures) -> RegexMatch {
    let whole = caps.get(0).unwrap();
    let captures = (1..caps.len())
        .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
        .collect();
    RegexMatch {
        text: whole.as_str().to_string(),
        codepoint_idx: s[..whole.start()].chars().count(),
        captures,
    }
}

/// Compile a `pattern` (a `String` or BSON `RegularExpression`) with optional
/// sibling `options` (a flag string). `i`/`m`/`s`/`x` map to the corresponding
/// flags; any other flag char is ignored (Python's `_re_flags` `.get(c, 0)`).
pub(crate) fn compile(pattern: &Bson, options: Option<&Bson>) -> Result<CompiledRegex, ()> {
    let (pat, embedded_flags): (&str, &str) = match pattern {
        Bson::String(s) => (s.as_str(), ""),
        Bson::RegularExpression(r) => (r.pattern.as_str(), r.options.as_str()),
        _ => return Err(()),
    };
    let opt_flags: &str = match options {
        None => "",
        Some(Bson::String(s)) => s.as_str(),
        Some(_) => return Err(()),
    };
    if pat.len() > MAX_REGEX_PATTERN_LEN {
        return Err(());
    }
    let (mut ci, mut ml, mut dotall, mut ext) = (false, false, false, false);
    for c in embedded_flags.chars().chain(opt_flags.chars()) {
        match c {
            'i' => ci = true,
            'm' => ml = true,
            's' => dotall = true,
            'x' => ext = true,
            _ => {}
        }
    }
    // PCRE's end anchors assert "end, or before a final newline"; the linear
    // engine has no lookahead to say that, so a pattern using them goes to the
    // backtracking engine with the anchor spelled out.
    if plain_literal(pat, ci, ext) {
        return Ok(CompiledRegex::Literal(pat.to_string()));
    }
    // Fast path: the linear engine handles almost every pattern -- but not one
    // whose end anchor was spelled out, which needs lookahead.
    let linear = pcre_rewrite(pat, ml, Boundary::Linear);
    if !linear.needs_lookaround {
        if let Ok(re) = RegexBuilder::new(&linear.pattern)
            .case_insensitive(ci)
            .multi_line(ml)
            .dot_matches_new_line(dotall)
            .ignore_whitespace(ext)
            .build()
        {
            return Ok(CompiledRegex::Linear(re));
        }
    }
    // Fallback: lookaround / backreferences via the backtracking engine. Flags
    // ride an inline group prefix since fancy-regex has no builder-flag API.
    let pat = pcre_rewrite(pat, ml, Boundary::Lookaround).pattern;
    let mut flagstr = String::new();
    for (on, ch) in [(ci, 'i'), (ml, 'm'), (dotall, 's'), (ext, 'x')] {
        if on {
            flagstr.push(ch);
        }
    }
    let full = if flagstr.is_empty() {
        pat
    } else {
        format!("(?{flagstr}){pat}")
    };
    fancy_regex::Regex::new(&full)
        .map(CompiledRegex::Fancy)
        .map_err(|_| ())
}

/// PCRE2's message for a pattern it would refuse, for the malformations measured
/// against mongod 8.2.11 (2026-10-01), which reports them as `51091 Regular
/// expression is invalid: <message>`. Scans left to right as PCRE does, so the
/// first fault wins. `None` when none of these applies.
pub(crate) fn pcre_compile_error(pat: &str) -> Option<String> {
    // PCRE2 checks the length before it parses anything.
    if pat.len() > MAX_REGEX_PATTERN_LEN {
        return Some(PATTERN_TOO_LONG.to_string());
    }
    let chars: Vec<char> = pat.chars().collect();
    let mut depth = 0usize;
    let mut names: Vec<String> = Vec::new();
    // Whether the previous token can take a quantifier.
    let mut repeatable = false;
    let mut i = 0;
    let quantifier_error = || Some("quantifier does not follow a repeatable item".to_string());
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                if i + 1 >= chars.len() {
                    return Some("\\ at end of pattern".to_string());
                }
                if chars[i + 1] == 'k' && chars.get(i + 2) == Some(&'<') {
                    let end = chars[i + 3..].iter().position(|&ch| ch == '>')?;
                    let name: String = chars[i + 3..i + 3 + end].iter().collect();
                    if !names.contains(&name) {
                        return Some("reference to non-existent subpattern".to_string());
                    }
                    i += 4 + end;
                } else {
                    i += 2;
                }
                repeatable = true;
                continue;
            }
            '[' => {
                let mut j = i + 1;
                if chars.get(j) == Some(&'^') {
                    j += 1;
                }
                if chars.get(j) == Some(&']') {
                    j += 1;
                }
                let mut prev: Option<char> = None;
                loop {
                    let Some(&ch) = chars.get(j) else {
                        return Some("missing terminating ] for character class".to_string());
                    };
                    if ch == ']' {
                        break;
                    }
                    if ch == '\\' {
                        prev = chars.get(j + 1).copied();
                        j += 2;
                        continue;
                    }
                    if ch == '-' {
                        if let (Some(lo), Some(&hi)) = (prev, chars.get(j + 1)) {
                            if hi != ']' && hi != '\\' && hi < lo {
                                return Some("range out of order in character class".to_string());
                            }
                        }
                    }
                    prev = Some(ch);
                    j += 1;
                }
                i = j + 1;
                repeatable = true;
                continue;
            }
            '(' => {
                depth += 1;
                if chars.get(i + 1) == Some(&'?') {
                    let rest: String = chars[i + 2..].iter().collect();
                    let named = rest.strip_prefix("P<").or_else(|| {
                        rest.strip_prefix('<')
                            .filter(|r| !r.starts_with(['=', '!']))
                    });
                    if let Some(after) = named {
                        let name: String = after
                            .chars()
                            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                            .collect();
                        if name.is_empty() {
                            return Some("subpattern name expected".to_string());
                        }
                        if names.contains(&name) {
                            return Some(
                                "two named subpatterns have the same name (PCRE2_DUPNAMES not set)"
                                    .to_string(),
                            );
                        }
                        names.push(name);
                    }
                    // Skip the group header so its `?` is not read as a
                    // quantifier: `?:` `?=` `?!` `?<=` `?<!`, a name, or flags.
                    let mut j = i + 2;
                    if let Some(after) = named {
                        let consumed = rest.len() - after.len();
                        let name_len = after.chars().take_while(|ch| *ch != '>').count();
                        j += consumed + name_len + 1;
                    } else {
                        while let Some(&ch) = chars.get(j) {
                            j += 1;
                            if matches!(ch, ':' | '=' | '!' | ')') {
                                if ch == ')' {
                                    j -= 1;
                                }
                                break;
                            }
                        }
                    }
                    i = j;
                    repeatable = false;
                    continue;
                }
                repeatable = false;
            }
            ')' => {
                if depth == 0 {
                    return Some("unmatched closing parenthesis".to_string());
                }
                depth -= 1;
                repeatable = true;
            }
            '|' => repeatable = false,
            '*' | '+' | '?' => {
                if !repeatable {
                    return quantifier_error();
                }
                // A lazy `?` or possessive `+` straight after a quantifier is
                // part of it, not a second quantifier.
                if matches!(chars.get(i + 1), Some('?') | Some('+')) {
                    i += 1;
                }
                repeatable = false;
            }
            '{' => {
                let close = chars[i..].iter().position(|&ch| ch == '}');
                let body: Option<String> = close.map(|e| chars[i + 1..i + e].iter().collect());
                let bounds = body.as_deref().and_then(|b| {
                    let (lo, hi) = b.split_once(',').unwrap_or((b, b));
                    Some((lo.parse::<u64>().ok()?, hi))
                });
                if let Some((lo, hi)) = bounds {
                    if !repeatable {
                        return quantifier_error();
                    }
                    if let Ok(hi) = hi.parse::<u64>() {
                        if hi < lo {
                            return Some("numbers out of order in {} quantifier".to_string());
                        }
                    }
                    i += close.unwrap_or(0) + 1;
                    repeatable = false;
                    continue;
                }
                repeatable = true;
            }
            _ => repeatable = true,
        }
        i += 1;
    }
    (depth > 0).then(|| "missing closing parenthesis".to_string())
}

/// PCRE's ASCII word characters, closed to case folding: under `i` the
/// `regex` crate folds `k` and `s` to the Kelvin sign and the long s, which
/// PCRE's `\w` never matches (mongod 8.2.11, 2026-10-22).
const WORD: &str = "(?-i:[0-9A-Za-z_])";
const NON_WORD: &str = "(?-i:[^0-9A-Za-z_])";
/// PCRE2's `\s`: space, tab, LF, VT, FF and CR -- not NBSP or U+0085.
const SPACE: &str = r"\t\n\x0B\x0C\r\x20";

/// How a rewritten pattern spells `\b` and `\B`.
#[derive(Clone, Copy, PartialEq)]
enum Boundary {
    /// The linear engine's ASCII boundary, `(?-u:\b)`.
    Linear,
    /// Lookarounds over [`WORD`], for the backtracking engine, which refuses
    /// `(?-u:...)`.
    Lookaround,
}

/// A pattern rewritten into PCRE's meaning for one engine.
struct Rewritten {
    pattern: String,
    /// Whether it uses lookaround, which only the backtracking engine has.
    needs_lookaround: bool,
}

/// The pattern with PCRE's meaning spelled out for the Rust engines.
///
/// **Character classes are ASCII.** mongod compiles with UTF but not UCP, so
/// `\w`, `\d`, `\s` and `\b` match ASCII only, with or without the `u` flag:
/// `^\w$` does not match "é", `^\d$` not an Arabic-Indic digit, `^\s$` not a
/// no-break space (measured 8.2.11, 2026-10-22). The Rust engines' classes
/// are Unicode, so each is replaced by its ASCII set, inside a bracket class
/// too (`[\w]`; a negated one nests, `[^0-9]`).
///
/// **End anchors.** Outside multiline mode PCRE's `$` matches at the end of
/// the subject OR before a newline that ends it; `\Z` always does. The
/// `regex` crate's `$` is only the very end, so `{s: /foo$/}` missed
/// `"foo\n"` and `$regexFind` reported no match, where mongod matches both
/// (measured 8.2.11, 2026-09-30); `\Z` did not compile at all and the query
/// was REFUSED. Both become `(?=\n?\z)`, which is PCRE's definition verbatim.
/// A `$` inside a character class is a literal and an escaped one is too.
/// When multiline is on -- by option or by an inline `(?m)` anywhere -- `$`
/// is left alone: the `regex` crate's multiline `$` is PCRE's.
fn pcre_rewrite(pat: &str, multiline: bool, boundary: Boundary) -> Rewritten {
    let inline_m = pat.match_indices("(?").any(|(i, _)| {
        pat[i + 2..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .any(|c| c == 'm')
    });
    let rewrite_dollar = !multiline && !inline_m;
    const END: &str = "(?=\\n?\\z)";
    let mut out = String::with_capacity(pat.len() + 16);
    let mut needs_lookaround = false;
    let mut in_class = false;
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('Z') if !in_class => {
                    out.push_str(END);
                    needs_lookaround = true;
                }
                Some(n @ ('b' | 'B')) if !in_class => match boundary {
                    Boundary::Linear => {
                        out.push_str(if n == 'b' { r"(?-u:\b)" } else { r"(?-u:\B)" })
                    }
                    Boundary::Lookaround => {
                        let (at_word, at_other) = (format!("(?={WORD})"), format!("(?!{WORD})"));
                        let (after_word, after_other) =
                            (format!("(?<={WORD})"), format!("(?<!{WORD})"));
                        out.push_str(&if n == 'b' {
                            format!("(?:{after_word}{at_other}|{after_other}{at_word})")
                        } else {
                            format!("(?:{after_word}{at_word}|{after_other}{at_other})")
                        });
                        needs_lookaround = true;
                    }
                },
                Some(n @ ('w' | 'W' | 'd' | 'D' | 's' | 'S')) => {
                    let set = match n.to_ascii_lowercase() {
                        'w' => "0-9A-Za-z_",
                        'd' => "0-9",
                        _ => SPACE,
                    };
                    let negated = n.is_ascii_uppercase();
                    match (in_class, negated, n.to_ascii_lowercase()) {
                        (false, false, 'w') => out.push_str(WORD),
                        (false, true, 'w') => out.push_str(NON_WORD),
                        (false, false, _) => out.push_str(&format!("[{set}]")),
                        (false, true, _) | (true, true, _) => out.push_str(&format!("[^{set}]")),
                        (true, false, _) => out.push_str(set),
                    }
                }
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            },
            '[' if !in_class => {
                in_class = true;
                out.push(c);
                // A `]` straight after `[` or `[^` is a literal member.
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().unwrap());
                }
                if chars.peek() == Some(&']') {
                    out.push(chars.next().unwrap());
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            '$' if !in_class && rewrite_dollar => {
                out.push_str(END);
                needs_lookaround = true;
            }
            _ => out.push(c),
        }
    }
    Rewritten {
        pattern: out,
        needs_lookaround,
    }
}

/// mongod's regex-to-regex equality: exact pattern, and options compared as a
/// SET.
///
/// Probed against 8.2.11 (2026-09-01): `/ab/im` equals `/ab/mi`, and `/ab/i`
/// does NOT equal `/ab/mi` -- so the option string is order-insensitive but not
/// subset-tolerant. This is the comparison behind a bare regex matching a
/// stored regex (`find({v: /ab/i})` over `{v: /ab/i}`), behind `$eq` with a
/// regex operand -- which is equality ONLY on mongod, never a pattern match --
/// and behind `$addToSet` membership.
pub(crate) fn regex_eq(a: &bson::Regex, b: &bson::Regex) -> bool {
    if a.pattern != b.pattern {
        return false;
    }
    let mut ao: Vec<char> = a.options.chars().collect();
    let mut bo: Vec<char> = b.options.chars().collect();
    ao.sort_unstable();
    ao.dedup();
    bo.sort_unstable();
    bo.dedup();
    ao == bo
}

/// The `(pattern, normalised options)` pair mongod ORDERS regexes by.
///
/// Probed 8.2.11 (2026-09-01): a mixed corpus sorts
/// `// < /A/ < /a/ < /a/i < /a/im < /a/m < /ab/ < /b/` -- pattern first, then
/// the option string. mongod stores options alphabetically sorted, so sorting
/// here is what makes an unsorted input compare the same as the stored form.
pub(crate) fn regex_sort_key(r: &bson::Regex) -> (&str, String) {
    let mut o: Vec<char> = r.options.chars().collect();
    o.sort_unstable();
    o.dedup();
    (r.pattern.as_str(), o.into_iter().collect())
}

#[cfg(test)]
mod pcre_anchor_tests {
    use super::compile;
    use bson::Bson;

    fn hits(pattern: &str, options: &str, subjects: &[&str]) -> Vec<bool> {
        let re = compile(
            &Bson::String(pattern.into()),
            Some(&Bson::String(options.into())),
        )
        .expect("compiles");
        subjects.iter().map(|s| re.is_match(s)).collect()
    }

    /// mongod 8.2.11, 2026-09-30.
    #[test]
    fn end_anchors_match_before_a_final_newline() {
        let subjects = ["foo", "foo\n", "foo\n\n", "foo\nbar"];
        assert_eq!(hits("foo$", "", &subjects), [true, true, false, false]);
        assert_eq!(hits("o$|z", "", &subjects), [true, true, false, false]);
        assert_eq!(hits("foo$\\n", "", &subjects), [false, true, false, false]);
        assert_eq!(hits("foo\\Z", "", &subjects), [true, true, false, false]);
        assert_eq!(hits("foo$", "s", &subjects), [true, true, false, false]);
        // Unchanged: multiline, a class member, an escaped dollar, `\z`.
        assert_eq!(hits("foo$", "m", &subjects), [true, true, true, true]);
        assert_eq!(hits("(?m)foo$", "", &subjects), [true, true, true, true]);
        assert_eq!(hits("[$]", "", &["$", "a"]), [true, false]);
        assert_eq!(hits("a\\$", "", &["a$", "a"]), [true, false]);
        assert_eq!(hits("foo\\z", "", &subjects), [true, false, false, false]);
    }

    #[test]
    fn find_reports_the_text_without_the_newline() {
        let re = compile(&Bson::String("o$".into()), None).unwrap();
        let m = re.find_first("foo\n").unwrap().expect("matches");
        assert_eq!((m.text.as_str(), m.codepoint_idx), ("o", 2));
    }

    /// mongod 8.2.11, 2026-10-22: PCRE without UCP, `u` flag or not.
    #[test]
    fn character_classes_are_ascii() {
        let s = ["é", "a", "\u{0661}", "1", "\u{a0}", "\u{b}", "\u{212a}"];
        assert_eq!(
            hits("^\\w$", "", &s),
            [false, true, false, true, false, false, false]
        );
        assert_eq!(
            hits("^\\W$", "", &s),
            [true, false, true, false, true, true, true]
        );
        assert_eq!(
            hits("^\\d$", "u", &s),
            [false, false, false, true, false, false, false]
        );
        assert_eq!(
            hits("^\\s$", "", &s),
            [false, false, false, false, false, true, false]
        );
        assert_eq!(
            hits("^[\\w]$", "", &s),
            [false, true, false, true, false, false, false]
        );
        assert_eq!(
            hits("^[^\\w]$", "", &s),
            [true, false, true, false, true, true, true]
        );
        // Case folding does not reach the Kelvin sign through `\w`.
        assert_eq!(
            hits("^\\w$", "i", &s),
            [false, true, false, true, false, false, false]
        );
        assert_eq!(hits("\\ba", "", &["éa", "ba"]), [true, false]);
        assert_eq!(hits("\\B", "", &["é"]), [true]);
        // The backtracking engine (a backreference) gets the same classes.
        assert_eq!(
            hits("^(\\w)\\1\\b", "", &["éé", "aa", "aab"]),
            [false, true, false]
        );
    }

    /// mongod's `$regexFindAll` scan, 8.2.11, 2026-10-22.
    #[test]
    fn find_all_scans_as_mongod() {
        let all = |pattern: &str, s: &str| -> Vec<(String, usize)> {
            compile(&Bson::String(pattern.into()), None)
                .unwrap()
                .find_all(s)
                .unwrap()
                .into_iter()
                .map(|m| (m.text, m.codepoint_idx))
                .collect()
        };
        let at = |v: &[(&str, usize)]| -> Vec<(String, usize)> {
            v.iter().map(|(t, i)| (t.to_string(), *i)).collect()
        };
        assert_eq!(all("", "abc"), at(&[("", 0), ("", 1), ("", 2)]));
        assert_eq!(all("", ""), at(&[("", 0)]));
        assert_eq!(all("", "é😀"), at(&[("", 0), ("", 1)]));
        assert_eq!(all("a*", "abc"), at(&[("a", 0), ("", 1), ("", 2)]));
        assert_eq!(all("a*", "aaa"), at(&[("aaa", 0)]));
        assert_eq!(all("c|", "abc"), at(&[("", 0), ("", 1), ("c", 2)]));
        assert_eq!(all("(?<=a)", "aa"), at(&[("", 1)]));
    }
}

#[cfg(test)]
mod regex_eq_tests {
    use super::regex_eq;

    fn r(pattern: &str, options: &str) -> bson::Regex {
        bson::Regex {
            pattern: pattern.into(),
            options: options.into(),
        }
    }

    #[test]
    fn options_compare_as_a_set() {
        assert!(regex_eq(&r("ab", "im"), &r("ab", "mi")));
        assert!(regex_eq(&r("ab", ""), &r("ab", "")));
        assert!(!regex_eq(&r("ab", "i"), &r("ab", "mi")));
        assert!(!regex_eq(&r("ab", "i"), &r("ab", "")));
    }

    #[test]
    fn sort_key_orders_by_pattern_then_options() {
        use super::regex_sort_key;
        let corpus = [
            r("", ""),
            r("A", ""),
            r("a", ""),
            r("a", "i"),
            r("a", "mi"),
            r("a", "m"),
        ];
        let mut keys: Vec<_> = corpus.iter().map(regex_sort_key).collect();
        keys.sort();
        let rendered: Vec<String> = keys.iter().map(|(p, o)| format!("/{p}/{o}")).collect();
        assert_eq!(rendered, ["//", "/A/", "/a/", "/a/i", "/a/im", "/a/m"]);
    }

    #[test]
    fn pattern_is_exact() {
        assert!(!regex_eq(&r("ab", "i"), &r("abc", "i")));
        assert!(!regex_eq(&r("ab", ""), &r("AB", "")));
    }
}

#[cfg(test)]
mod regex_key_agreement {
    /// The index-entry encoder and the in-memory comparator must order two
    /// regexes identically -- the failure mode is an index changing the sort
    /// answer, which is how the JavaScript rank bug was found.
    #[test]
    fn sortkey_bytes_and_cmp_agree() {
        use bson::Bson;
        let corpus = [
            ("b", ""),
            ("a", "m"),
            ("a", ""),
            ("ab", ""),
            ("a", "mi"),
            ("A", ""),
            ("a", "i"),
            ("", ""),
        ];
        let vals: Vec<Bson> = corpus
            .iter()
            .map(|(p, o)| {
                Bson::RegularExpression(bson::Regex {
                    pattern: (*p).into(),
                    options: (*o).into(),
                })
            })
            .collect();

        let mut by_cmp: Vec<usize> = (0..vals.len()).collect();
        by_cmp.sort_by(|&i, &j| crate::order::cmp(&vals[i], &vals[j]));

        let mut by_bytes: Vec<usize> = (0..vals.len()).collect();
        by_bytes.sort_by_key(|&i| crate::sortkey::encode_value(&vals[i], None).unwrap());

        assert_eq!(by_cmp, by_bytes);
    }
}

#[cfg(test)]
mod pcre_error_tests {
    use super::pcre_compile_error;

    /// PCRE's messages for the malformations measured against mongod 8.2.11.
    #[test]
    fn pcre_compile_errors_match_mongod() {
        for (pat, want) in [
            ("(", "missing closing parenthesis"),
            (")", "unmatched closing parenthesis"),
            ("[", "missing terminating ] for character class"),
            ("*", "quantifier does not follow a repeatable item"),
            ("a\\", "\\ at end of pattern"),
            ("a{2,1}", "numbers out of order in {} quantifier"),
            ("(?<", "subpattern name expected"),
            ("[z-a]", "range out of order in character class"),
            ("a**", "quantifier does not follow a repeatable item"),
            (
                "(?P<n>a)(?P<n>b)",
                "two named subpatterns have the same name (PCRE2_DUPNAMES not set)",
            ),
            ("\\k<x>", "reference to non-existent subpattern"),
        ] {
            assert_eq!(pcre_compile_error(pat).as_deref(), Some(want), "{pat}");
        }
        for ok in [
            "a*?",
            "a+?",
            "(?:a)",
            "(?=a)",
            "(?<n>a)\\k<n>",
            "[a-z]",
            "a{2,3}",
            "x{",
        ] {
            assert_eq!(pcre_compile_error(ok), None, "{ok}");
        }
    }
}

#[cfg(test)]
mod length_and_literal_tests {
    use super::*;

    /// mongod 8.2.11: 32,764 bytes compile, 32,765 do not -- counted in
    /// UTF-8 bytes, not characters.
    #[test]
    fn the_length_limit_is_mongods() {
        let ok = Bson::String("a".repeat(32_764));
        assert!(compile(&ok, None).is_ok());
        let long = "a".repeat(32_765);
        assert!(compile(&Bson::String(long.clone()), None).is_err());
        assert_eq!(pcre_compile_error(&long).as_deref(), Some(PATTERN_TOO_LONG));
        let wide = "\u{e9}".repeat(16_383); // 32,766 bytes
        assert_eq!(pcre_compile_error(&wide).as_deref(), Some(PATTERN_TOO_LONG));
        assert_eq!(pcre_compile_error(&"\u{e9}".repeat(16_382)), None);
    }

    #[test]
    fn a_long_literal_is_a_substring_search() {
        let re = compile(&Bson::String("a".repeat(32_764)), None).unwrap();
        assert!(matches!(re, CompiledRegex::Literal(_)));
        assert!(re.is_match(&"a".repeat(40_000)));
        assert!(!re.is_match(&"a".repeat(32_763)));
        let ab = compile(&Bson::String("ab".into()), None).unwrap();
        let all = ab.find_all("xabab\u{e9}ab").unwrap();
        let idx: Vec<usize> = all.iter().map(|m| m.codepoint_idx).collect();
        assert_eq!(idx, vec![1, 3, 6]);
        // A flag or a metacharacter keeps the regex engine.
        let ci = compile(&Bson::String("ab".into()), Some(&Bson::String("i".into()))).unwrap();
        assert!(ci.is_match("AB"));
        assert!(!matches!(
            compile(&Bson::String("a.b".into()), None).unwrap(),
            CompiledRegex::Literal(_)
        ));
    }
}
