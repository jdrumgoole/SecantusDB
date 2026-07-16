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
