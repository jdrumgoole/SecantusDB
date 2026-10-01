//! Shared regex compilation + matching, used by both `query::$regex` and the
//! `$regexMatch` / `$regexFind` / `$regexFindAll` aggregation expressions.
//!
//! Mirrors `secantus.query` / `secantus.expressions` regex handling: the linear
//! `regex` crate is the fast path for almost every pattern; a pattern it can't
//! compile (lookaround / backreferences) falls back to the backtracking
//! `fancy-regex`. Only a non-string pattern/options, a pattern over the length
//! cap, or one neither engine compiles signals defer (`Err(())`).
//!
//! `is_match` (Python `re.search` truthiness) works on both engines. Positional
//! find (`find_first` / `find_all`, backing `$regexFind` / `$regexFindAll`) is
//! served **only** by the linear engine — its leftmost-first match + capture
//! semantics align with Python `re`, whereas the backtracking engine's capture
//! behaviour is a parity risk, so a fancy-only pattern defers those two.

use bson::Bson;
use regex::{Regex as LinearRegex, RegexBuilder};

/// Hard cap on user-supplied regex pattern length, mirroring
/// `secantus.query._MAX_REGEX_PATTERN_LEN` / the `$regex*` expression cap.
pub(crate) const MAX_REGEX_PATTERN_LEN: usize = 1000;

/// A compiled regex from whichever engine could build it. Both run unanchored
/// (`re.search`) semantics.
pub(crate) enum CompiledRegex {
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
            CompiledRegex::Linear(re) => Ok(re.captures(s).map(|c| to_match(s, &c))),
            CompiledRegex::Fancy(re) => match re.captures(s) {
                Ok(Some(c)) => Ok(Some(to_match_fancy(s, &c))),
                Ok(None) => Ok(None),
                Err(_) => Err(()), // backtrack limit / engine error -> defer
            },
        }
    }

    /// All non-overlapping matches left-to-right (Python `re.finditer`). `Err(())`
    /// → defer (a backtrack-limit error from the fancy engine).
    pub(crate) fn find_all(&self, s: &str) -> Result<Vec<RegexMatch>, ()> {
        match self {
            CompiledRegex::Linear(re) => Ok(re.captures_iter(s).map(|c| to_match(s, &c)).collect()),
            CompiledRegex::Fancy(re) => {
                let mut out = Vec::new();
                for caps in re.captures_iter(s) {
                    out.push(to_match_fancy(s, &caps.map_err(|_| ())?));
                }
                Ok(out)
            }
        }
    }
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
    let rewritten = pcre_end_anchors(pat, ml);
    let pat: &str = rewritten.as_deref().unwrap_or(pat);
    // Fast path: the linear engine handles almost every pattern -- but not a
    // rewritten one, which needs lookahead.
    if rewritten.is_none() {
        if let Ok(re) = RegexBuilder::new(pat)
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
    let mut flagstr = String::new();
    for (on, ch) in [(ci, 'i'), (ml, 'm'), (dotall, 's'), (ext, 'x')] {
        if on {
            flagstr.push(ch);
        }
    }
    let full = if flagstr.is_empty() {
        pat.to_string()
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

/// PCRE's lookahead-free spelling of `$` and `\Z`, for a pattern that uses
/// them -- `None` when it uses neither.
///
/// Outside multiline mode PCRE's `$` matches at the end of the subject OR
/// before a newline that ends it; `\Z` always does. The `regex` crate's `$` is
/// only the very end, so `{s: /foo$/}` missed `"foo\n"` and `$regexFind`
/// reported no match, where mongod matches both (measured 8.2.11,
/// 2026-09-30); `\Z` did not compile at all and the query was REFUSED. Both
/// become `(?=\n?\z)`, which is PCRE's definition verbatim.
///
/// A `$` inside a character class is a literal and an escaped one is too. When
/// multiline is on -- by option or by an inline `(?m)` anywhere -- `$` is left
/// alone: the `regex` crate's multiline `$` is PCRE's.
fn pcre_end_anchors(pat: &str, multiline: bool) -> Option<String> {
    let inline_m = pat.match_indices("(?").any(|(i, _)| {
        pat[i + 2..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .any(|c| c == 'm')
    });
    let rewrite_dollar = !multiline && !inline_m;
    const END: &str = "(?=\\n?\\z)";
    let mut out = String::with_capacity(pat.len() + 16);
    let mut changed = false;
    let mut in_class = false;
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('Z') if !in_class => {
                    out.push_str(END);
                    changed = true;
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
                changed = true;
            }
            _ => out.push(c),
        }
    }
    changed.then_some(out)
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
