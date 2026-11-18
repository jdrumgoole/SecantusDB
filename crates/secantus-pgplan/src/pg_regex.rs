//! A PostgreSQL regular expression, compiled by whichever engine can run it.
//!
//! The linear `regex` crate runs almost every pattern, and runs it in linear
//! time. It refuses a back reference (`(ab)\1`), which PostgreSQL's ARE
//! engine runs, so a pattern it refuses is compiled again with the
//! backtracking `fancy-regex` (RP-1022), as `secantus-core` does for a
//! WHERE's `$regex` and `colstats` for the planner's estimate (RP-423).

use crate::{Error, Result};

/// The byte span of each group of one match (group 0 is the whole match),
/// `None` for a group that did not take part.
pub(crate) type Groups = Vec<Option<(usize, usize)>>;

pub(crate) enum PgRegex {
    Linear(regex::Regex),
    Backtracking(fancy_regex::Regex),
}

/// The backtracking engine's own failure (its backtrack limit) as an error.
fn engine_error(e: fancy_regex::Error) -> Error {
    Error::InvalidRegex(format!("invalid regular expression: {e}"))
}

impl PgRegex {
    /// Compile `source` (already in the `regex` crate's syntax, inline flags
    /// included). `Err` carries the linear engine's message when neither
    /// engine compiles it.
    pub(crate) fn new(source: &str) -> std::result::Result<Self, regex::Error> {
        match regex::Regex::new(source) {
            Ok(re) => Ok(PgRegex::Linear(re)),
            Err(e) => fancy_regex::Regex::new(source)
                .map(PgRegex::Backtracking)
                .map_err(|_| e),
        }
    }

    pub(crate) fn is_match(&self, s: &str) -> Result<bool> {
        match self {
            PgRegex::Linear(re) => Ok(re.is_match(s)),
            PgRegex::Backtracking(re) => re.is_match(s).map_err(engine_error),
        }
    }

    /// The first match's groups, or `None`.
    pub(crate) fn captures(&self, s: &str) -> Result<Option<Groups>> {
        match self {
            PgRegex::Linear(re) => Ok(re
                .captures(s)
                .map(|c| c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect())),
            PgRegex::Backtracking(re) => Ok(re
                .captures(s)
                .map_err(engine_error)?
                .map(|c| c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect())),
        }
    }

    /// Every non-overlapping match, left to right.
    pub(crate) fn captures_all(&self, s: &str) -> Result<Vec<Groups>> {
        match self {
            PgRegex::Linear(re) => Ok(re
                .captures_iter(s)
                .map(|c| c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect())
                .collect()),
            PgRegex::Backtracking(re) => re
                .captures_iter(s)
                .map(|c| {
                    c.map(|c| c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect())
                        .map_err(engine_error)
                })
                .collect(),
        }
    }

    /// `s` with the first match (or every one, `all`) replaced by
    /// `template`, written in the `regex` crate's replacement syntax
    /// (`${n}`, `$$`).
    pub(crate) fn replace(&self, s: &str, template: &str, all: bool) -> Result<String> {
        let matches = if all {
            self.captures_all(s)?
        } else {
            self.captures(s)?.into_iter().collect()
        };
        let mut out = String::new();
        let mut last = 0;
        for groups in matches {
            let Some((start, end)) = groups[0] else {
                continue;
            };
            out.push_str(&s[last..start]);
            expand(s, &groups, template, &mut out);
            last = end;
        }
        out.push_str(&s[last..]);
        Ok(out)
    }
}

/// The text of group `i`, if it took part.
pub(crate) fn group_text<'a>(s: &'a str, groups: &Groups, i: usize) -> Option<&'a str> {
    groups.get(i).copied().flatten().map(|(a, b)| &s[a..b])
}

/// Append `template` with each `${n}` replaced by group `n` and `$$` by `$`.
pub(crate) fn expand(s: &str, groups: &Groups, template: &str, out: &mut String) {
    let mut rest = template;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            out.push('$');
            rest = r;
        } else if let Some((n, r)) = rest
            .strip_prefix('{')
            .and_then(|r| r.split_once('}'))
            .and_then(|(n, r)| Some((n.parse::<usize>().ok()?, r)))
        {
            out.push_str(group_text(s, groups, n).unwrap_or(""));
            rest = r;
        } else {
            out.push('$');
        }
    }
    out.push_str(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_back_reference_runs_on_the_backtracking_engine() {
        let re = PgRegex::new(r"(ab)\1").unwrap();
        assert!(matches!(re, PgRegex::Backtracking(_)));
        assert!(re.is_match("abab").unwrap());
        assert!(!re.is_match("abac").unwrap());
        assert_eq!(re.replace("xababy", "[${1}]", false).unwrap(), "x[ab]y");
        assert_eq!(re.replace("ababab", "$$", true).unwrap(), "$ab");
    }

    #[test]
    fn a_plain_pattern_stays_linear() {
        assert!(matches!(PgRegex::new("a+b").unwrap(), PgRegex::Linear(_)));
        assert!(PgRegex::new("(").is_err());
    }
}
