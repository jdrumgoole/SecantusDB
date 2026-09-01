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
    // Fast path: the linear engine handles almost every pattern.
    if let Ok(re) = RegexBuilder::new(pat)
        .case_insensitive(ci)
        .multi_line(ml)
        .dot_matches_new_line(dotall)
        .ignore_whitespace(ext)
        .build()
    {
        return Ok(CompiledRegex::Linear(re));
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
mod regex_eq_tests {
    use super::regex_eq;

    fn r(pattern: &str, options: &str) -> bson::Regex {
        bson::Regex { pattern: pattern.into(), options: options.into() }
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
        let corpus = [r("", ""), r("A", ""), r("a", ""), r("a", "i"), r("a", "mi"), r("a", "m")];
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
                Bson::RegularExpression(bson::Regex { pattern: (*p).into(), options: (*o).into() })
            })
            .collect();

        let mut by_cmp: Vec<usize> = (0..vals.len()).collect();
        by_cmp.sort_by(|&i, &j| crate::order::cmp(&vals[i], &vals[j]));

        let mut by_bytes: Vec<usize> = (0..vals.len()).collect();
        by_bytes.sort_by_key(|&i| crate::sortkey::encode_value(&vals[i], None).unwrap());

        assert_eq!(by_cmp, by_bytes);
    }
}
