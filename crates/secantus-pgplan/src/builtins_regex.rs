//! The regular-expression functions `builtins` answers, and the checks every
//! regular-expression function's arguments get: PostgreSQL validates the
//! numbers (start, n, end option, subexpression) and then the options, and
//! refuses a letter that is no option.

use bson::Bson;

use crate::builtins::{int, text};
use crate::{Error, Result};

/// The options a regular-expression function takes, as PostgreSQL's
/// `parse_re_flags` reads them. `g` is the caller's to allow.
pub(crate) fn check_regex_flags(flags: &str) -> Result<()> {
    match flags.chars().find(|c| !"gbceimnpqstwx".contains(*c)) {
        Some(c) => Err(Error::Sqlstate(
            "22023",
            format!("invalid regular expression option: \"{c}\""),
        )),
        None => Ok(()),
    }
}

/// Where a regular-expression function keeps its options, by name and
/// argument count. `types` tells `regexp_replace`'s two four-argument forms
/// apart.
pub(crate) fn regex_flags_position(name: &str, types: &[String]) -> Option<usize> {
    let n = types.len();
    let at = match (name, n) {
        ("regexp_like" | "regexp_match" | "regexp_matches", 3) => 2,
        ("regexp_split_to_array" | "regexp_split_to_table", 3) => 2,
        ("regexp_count", 4) => 3,
        ("regexp_substr", 5 | 6) => 4,
        ("regexp_instr", 6 | 7) => 5,
        // With a start position `regexp_replace` is this module's own, and
        // checks its numbers before its options there.
        ("regexp_replace", 4) if !matches!(types[3].as_str(), "int2" | "int4" | "int8") => 3,
        _ => return None,
    };
    Some(at)
}

/// The numeric arguments of `regexp_count` / `regexp_substr` /
/// `regexp_instr`, checked in the order PostgreSQL checks them, which is
/// before it reads the options.
pub(crate) fn check_regex_numbers(name: &str, args: &[Bson]) -> Result<()> {
    // (argument position, parameter name, least value allowed)
    let checks: &[(usize, &str, i64)] = match name {
        "regexp_count" => &[(2, "start", 1)],
        "regexp_substr" => &[(2, "start", 1), (3, "n", 1), (5, "subexpr", 0)],
        "regexp_instr" => &[
            (2, "start", 1),
            (3, "n", 1),
            (4, "endoption", 0),
            (6, "subexpr", 0),
        ],
        _ => return Ok(()),
    };
    for (at, param, least) in checks {
        let Some(value) = args.get(*at) else { continue };
        let Ok(n) = int(value, "integer") else {
            continue;
        };
        if n < *least || (*param == "endoption" && n > 1) {
            return Err(bad_parameter(param, n));
        }
    }
    Ok(())
}

fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex> {
    regex::RegexBuilder::new(&crate::scalar::pg_regex_source(pattern))
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('n') || flags.contains('m') || flags.contains('w'))
        .dot_matches_new_line(!(flags.contains('n') || flags.contains('m') || flags.contains('p')))
        .ignore_whitespace(flags.contains('x'))
        .build()
        .map_err(|_| Error::InvalidRegex(format!("invalid regular expression: \"{pattern}\"")))
}

/// PostgreSQL's replacement text as the `regex` crate's: `\1` is a group,
/// `\&` the whole match, and a `$` is itself.
fn replacement_for(replacement: &str) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(*d);
                    out.push('}');
                    chars.next();
                    continue;
                }
                Some('&') => {
                    out.push_str("${0}");
                    chars.next();
                    continue;
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                    continue;
                }
                _ => {}
            }
        } else if c == '$' {
            out.push_str("$$");
            continue;
        }
        out.push(c);
    }
    out
}

fn bad_parameter(name: &str, value: i64) -> Error {
    Error::Sqlstate(
        "22023",
        format!("invalid value for parameter \"{name}\": {value}"),
    )
}

/// `regexp_replace(source, pattern, replacement, start [, n [, flags]])`:
/// the `n`th match at or after character `start` is replaced, or every one
/// when `n` is 0. With `n` given, a `g` in the flags changes nothing.
pub(crate) fn regexp_replace_from(args: &[Bson]) -> Result<Bson> {
    let (source, pattern, replacement) = (text(&args[0]), text(&args[1]), text(&args[2]));
    let start = int(&args[3], "integer")?;
    if start < 1 {
        return Err(bad_parameter("start", start));
    }
    let n = match args.get(4) {
        Some(v) => {
            let n = int(v, "integer")?;
            if n < 0 {
                return Err(bad_parameter("n", n));
            }
            Some(n)
        }
        None => None,
    };
    let flags = args.get(5).map(text).unwrap_or_default();
    check_regex_flags(&flags)?;
    let re = build_regex(&pattern, &flags)?;
    let from = match source.char_indices().nth((start - 1) as usize) {
        Some((byte, _)) => byte,
        None => source.len(),
    };
    let replacement = replacement_for(&replacement);
    let (head, tail) = source.split_at(from);
    let which = match n {
        Some(n) => n,
        None if flags.contains('g') => 0,
        None => 1,
    };
    let mut out = String::from(head);
    let mut last = 0;
    for (i, caps) in re.captures_iter(tail).enumerate() {
        let m = caps.get(0).expect("group 0 is the match");
        if which != 0 && i as i64 + 1 != which {
            continue;
        }
        out.push_str(&tail[last..m.start()]);
        caps.expand(&replacement, &mut out);
        last = m.end();
        if which != 0 {
            break;
        }
    }
    out.push_str(&tail[last..]);
    Ok(Bson::String(out))
}

/// `regexp_match`: the first match's capture groups, or the whole match
/// when the pattern has none; NULL when nothing matches.
pub(crate) fn regexp_match(args: &[Bson]) -> Result<Bson> {
    let (source, pattern) = (text(&args[0]), text(&args[1]));
    let flags = args.get(2).map(text).unwrap_or_default();
    check_regex_flags(&flags)?;
    if flags.contains('g') {
        return Err(Error::Sqlstate(
            "22023",
            "regexp_match() does not support the \"global\" option".into(),
        ));
    }
    let re = build_regex(&pattern, &flags)?;
    let Some(caps) = re.captures(&source) else {
        return Ok(Bson::Null);
    };
    let group = |i: usize| {
        caps.get(i)
            .map_or(Bson::Null, |m| Bson::String(m.as_str().to_string()))
    };
    Ok(Bson::Array(if caps.len() == 1 {
        vec![group(0)]
    } else {
        (1..caps.len()).map(group).collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn a_letter_that_is_no_regex_option_is_refused() {
        assert!(check_regex_flags("gi").is_ok());
        assert!(check_regex_flags("").is_ok());
        assert!(check_regex_flags("z").is_err());
        assert_eq!(
            regex_flags_position("regexp_like", &types(&["text"; 3])),
            Some(2)
        );
        assert_eq!(
            regex_flags_position("regexp_like", &types(&["text"; 2])),
            None
        );
        // The fourth argument of regexp_replace is its options, or a start.
        assert_eq!(
            regex_flags_position("regexp_replace", &types(&["text"; 4])),
            Some(3)
        );
        assert_eq!(
            regex_flags_position("regexp_replace", &types(&["text", "text", "text", "int4"])),
            None
        );
    }

    #[test]
    fn regexp_replace_from_a_start_replaces_the_nth_match() {
        let t = types(&["text", "text", "text", "int4", "int4"]);
        let run = |start: i32, n: i32| {
            let args = [
                Bson::String("abcabcabc".into()),
                Bson::String("b".into()),
                Bson::String("X".into()),
                Bson::Int32(start),
                Bson::Int32(n),
            ];
            match crate::builtins::call("regexp_replace", &args, &t, false) {
                Ok(Bson::String(s)) => s,
                other => format!("{other:?}"),
            }
        };
        assert_eq!(run(1, 1), "aXcabcabc");
        assert_eq!(run(1, 2), "abcaXcabc");
        assert_eq!(run(1, 0), "aXcaXcaXc");
        assert_eq!(run(4, 1), "abcaXcabc");
        assert_eq!(run(1, 9), "abcabcabc");
    }
}
