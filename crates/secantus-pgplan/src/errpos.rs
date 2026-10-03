//! The `P` (position) field of an error: the 1-based character offset of the
//! token PostgreSQL's error cursor points at (`LINE 1: select nocol ...`
//! with a caret under `nocol`).
//!
//! PostgreSQL knows the token from the parse tree node the error is about.
//! A raising site here that has the node records its location
//! (`set_error_location`); otherwise the token is found in the statement's
//! own tokens from what the message names -- the column, the function, the
//! literal -- which is the same token PostgreSQL points at whenever the name
//! occurs once, and its first mention otherwise.

use super::*;

struct Tok<'a> {
    start: usize,
    text: &'a str,
}

fn unquote_ident(text: &str) -> String {
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => text.to_ascii_lowercase(),
    }
}

fn quoted_between<'a>(m: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = m.strip_prefix(prefix)?;
    let rest = rest.strip_prefix('"')?;
    rest.split_once('"').map(|(name, _)| name)
}

/// The position in `sql` of an error with this SQLSTATE and primary
/// message, when it can be found.
pub fn error_position(sql: &str, sqlstate: &str, message: &str) -> Option<usize> {
    let location = take_error_location();
    // `CREATE CAST` / `DROP CAST` resolve their types and function by name,
    // outside any expression, and PostgreSQL reports no position for them.
    let head: String = sql
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if head == "create cast" || head == "drop cast" {
        return None;
    }
    let scanned = pg_query::scan(sql).ok()?;
    let toks: Vec<Tok> = scanned
        .tokens
        .iter()
        .filter_map(|t| {
            let (s, e) = (t.start as usize, t.end as usize);
            Some(Tok {
                start: s,
                text: sql.get(s..e)?,
            })
        })
        .collect();
    let pos = |start: usize| Some(sql.get(..start)?.chars().count() + 1);
    if let Some(loc) = location.and_then(|l| usize::try_from(l).ok()) {
        if toks.iter().any(|t| t.start == loc) {
            return pos(loc);
        }
    }
    let ident_at = |name: &str| toks.iter().position(|t| unquote_ident(t.text) == name);
    let m = message.split('\n').next().unwrap_or("");
    // A set operation whose column types cannot be unified points at the
    // RIGHT arm's expression (`select null::text union select 1`, under `1`).
    if sqlstate == "42804" && m.contains(" types ") && m.ends_with("cannot be matched") {
        let tree = pg_query::parse(sql).ok()?;
        let stmt = tree.protobuf.stmts.first()?.stmt.as_ref()?;
        let Some(N::SelectStmt(sel)) = stmt.node.as_ref() else {
            return None;
        };
        let right = sel.rarg.as_deref()?;
        let loc = right
            .target_list
            .first()
            .and_then(|t| match t.node.as_ref() {
                Some(N::ResTarget(rt)) => rt.val.as_deref().and_then(crate::expr_location),
                _ => None,
            })?;
        return pos(usize::try_from(loc).ok()?);
    }
    match sqlstate {
        // `could not identify column "b" in record data type`: PostgreSQL
        // points at the record, the `c` of `(c).b`.
        "42703" if m.starts_with("could not identify column ") => {
            let field = m
                .strip_prefix("could not identify column \"")?
                .split('"')
                .next()?;
            let i = toks.iter().enumerate().position(|(i, t)| {
                unquote_ident(t.text) == field
                    && i >= 4
                    && toks[i - 1].text == "."
                    && toks[i - 2].text == ")"
                    && toks[i - 4].text == "("
            })?;
            pos(toks[i - 3].start)
        }
        // `ORDER BY position 5 is not in select list`: at the number.
        "42P10" if m.ends_with(" is not in select list") => {
            let (clause, rest) = m.split_once(" position ")?;
            let n = rest.split(' ').next()?;
            let i = toks.windows(3).position(|w| {
                w[0].text
                    .eq_ignore_ascii_case(clause.rsplit(' ').next().unwrap_or(""))
                    && w[1].text.eq_ignore_ascii_case("by")
            });
            // The number among the clause's items: the first token equal to it
            // after the clause keyword.
            let from = i.map_or(0, |i| i + 2);
            pos(toks.iter().skip(from).find(|t| t.text == n)?.start)
        }
        // `invalid reference to FROM-clause entry for table "t"`: at `t.`.
        "42P01" if m.starts_with("invalid reference to FROM-clause entry") => {
            let name = quoted_between(m, "invalid reference to FROM-clause entry for table ")?;
            let i = toks.iter().enumerate().position(|(i, t)| {
                unquote_ident(t.text) == name && toks.get(i + 1).is_some_and(|n| n.text == ".")
            })?;
            pos(toks[i].start)
        }
        "42703" | "42803" => {
            // `column "x" does not exist`, or `column t.x does not exist`:
            // a qualified reference points at its qualifier. And `column "x"
            // must appear in the GROUP BY clause ...`: its first mention.
            if sqlstate == "42803" && !m.contains(" must appear in the GROUP BY clause") {
                return None;
            }
            let name = quoted_between(m, "column ")
                .map(str::to_string)
                .or_else(|| {
                    m.strip_prefix("column ")?
                        .strip_suffix(" does not exist")
                        .map(str::to_string)
                })?;
            let parts: Vec<&str> = name.split('.').collect();
            let last = parts.last()?;
            let qualified = toks.iter().enumerate().position(|(i, t)| {
                unquote_ident(t.text) == *last
                    && (parts.len() == 1
                        || (i >= 2
                            && toks[i - 1].text == "."
                            && unquote_ident(toks[i - 2].text) == parts[parts.len() - 2]))
            });
            match qualified {
                Some(i) => pos(toks[if parts.len() > 1 { i - 2 } else { i }].start),
                // An ungrouped column is NAMED qualified (`t.a`) however it
                // was written: a bare mention is the one PostgreSQL means.
                None if sqlstate == "42803" => {
                    pos(toks.iter().find(|t| unquote_ident(t.text) == *last)?.start)
                }
                None => None,
            }
        }
        "42P01" => {
            let full = quoted_between(m, "relation ")
                .or_else(|| quoted_between(m, "missing FROM-clause entry for table "))?;
            let name = full.rsplit('.').next()?;
            match ident_at(name) {
                Some(i) => pos(toks[i].start),
                // A relation named by a string (`'t'::regclass`): at the
                // literal.
                None => {
                    let quoted = format!("'{}'", full.replace('\'', "''"));
                    pos(toks.iter().find(|t| t.text == quoted)?.start)
                }
            }
        }
        "42704" => {
            let name = quoted_between(m, "type ")?;
            let name = name.rsplit('.').next()?;
            pos(toks[ident_at(name)?].start)
        }
        // A procedure called in an expression: at its name.
        "42809" if m.ends_with(" is a procedure") => {
            let name = m.split('(').next()?.rsplit('.').next()?;
            let i = toks.iter().enumerate().position(|(i, t)| {
                unquote_ident(t.text) == name && toks.get(i + 1).is_some_and(|n| n.text == "(")
            })?;
            pos(toks[i].start)
        }
        // `CALL f()` over a FUNCTION: at the name, as a missing procedure.
        "42809" if m.ends_with(" is not a procedure") => {
            if !toks
                .first()
                .is_some_and(|t| t.text.eq_ignore_ascii_case("call"))
            {
                return None;
            }
            let name = m.split('(').next()?.rsplit('.').next()?;
            let i = toks.iter().enumerate().position(|(i, t)| {
                unquote_ident(t.text) == name && toks.get(i + 1).is_some_and(|n| n.text == "(")
            })?;
            pos(toks[i].start)
        }
        "42883" => {
            if let Some(call) = m
                .strip_prefix("function ")
                .or_else(|| m.strip_prefix("procedure "))
            {
                // A function a DROP / ALTER / COMMENT names is looked up
                // without a parse position.
                if toks.first().is_some_and(|t| {
                    ["drop", "alter", "comment"]
                        .iter()
                        .any(|k| t.text.eq_ignore_ascii_case(k))
                }) {
                    return None;
                }
                // Nor is a trigger's `EXECUTE FUNCTION f()`.
                if toks.windows(2).any(|w| {
                    w[0].text.eq_ignore_ascii_case("execute")
                        && ["function", "procedure"]
                            .iter()
                            .any(|k| w[1].text.eq_ignore_ascii_case(k))
                }) {
                    return None;
                }
                let name = call.split('(').next()?.rsplit('.').next()?;
                let i = toks.iter().enumerate().position(|(i, t)| {
                    unquote_ident(t.text) == name && toks.get(i + 1).is_some_and(|n| n.text == "(")
                })?;
                return pos(toks[i].start);
            }
            let rest = m.strip_prefix("operator does not exist: ")?;
            let op = rest
                .split(' ')
                .find(|w| !w.is_empty() && w.chars().all(|c| "+-*/<>=~!@#%^&|`?".contains(c)))?;
            let mut hits = toks.iter().filter(|t| t.text == op);
            let first = hits.next()?;
            hits.next().is_none().then(|| pos(first.start))?
        }
        // A date/time literal that does not read: at the literal.
        "22007" | "22008" => {
            let (_, value) = m.rsplit_once(": \"")?;
            let value = value.strip_suffix('"')?;
            let lit = format!("'{}'", value.replace('\'', "''"));
            pos(toks.iter().find(|t| t.text == lit)?.start)
        }
        "22P02" => {
            // `invalid input syntax for type integer: "abc"`: the literal.
            let (_, value) = m.rsplit_once(": \"")?;
            let value = value.strip_suffix('"')?;
            let lit = format!("'{}'", value.replace('\'', "''"));
            pos(toks.iter().find(|t| t.text == lit)?.start)
        }
        "42601" => {
            if m == "syntax error at end of input" {
                return Some(sql.trim_end().chars().count() + 1);
            }
            let near = quoted_between(m, "syntax error at or near ")?;
            pos(toks.iter().find(|t| t.text == near)?.start)
        }
        "42804" => {
            // `argument of WHERE must be type boolean`: the clause's first
            // token.
            let clause = m.strip_prefix("argument of ")?.split(' ').next()?;
            let mut hits = toks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.text.eq_ignore_ascii_case(clause));
            let (i, _) = hits.next()?;
            if hits.next().is_some() {
                return None;
            }
            pos(toks.get(i + 1)?.start)
        }
        _ => None,
    }
}
