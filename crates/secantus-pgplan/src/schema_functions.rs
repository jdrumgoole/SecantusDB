//! `information_schema`'s helper functions (`_pg_char_max_length`,
//! `_pg_truetypid`, ...): SQL functions whose bodies a call is replaced by,
//! as PostgreSQL's `inline_function` replaces a SQL function's call
//! (`pg15_information_schema_functions.tsv`, PostgreSQL 15.19's
//! `pg_get_function_sqlbody`; `_pg_expandarray`, the one set-returning
//! helper, is a built-in here).
//!
//! The substitution is on the SQL text: each argument, parenthesised, for its
//! parameter's name or `$n` in the body, and `($n).field` over a whole-row
//! argument `t.*` as `t.field`. A body that calls another helper is inlined
//! again.

use std::collections::HashMap;
use std::sync::OnceLock;

use pg_query::protobuf::Token;

const FUNCTIONS: &str = include_str!("pg15_information_schema_functions.tsv");

struct Helper {
    params: Vec<String>,
    nargs: usize,
    body: String,
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn helpers() -> &'static HashMap<String, Helper> {
    static MAP: OnceLock<HashMap<String, Helper>> = OnceLock::new();
    MAP.get_or_init(|| {
        FUNCTIONS
            .lines()
            .filter_map(|l| {
                let c: Vec<&str> = l.splitn(4, '\t').collect();
                let [name, params, nargs, body] = c[..] else {
                    return None;
                };
                Some((
                    name.to_string(),
                    Helper {
                        params: params
                            .split(',')
                            .filter(|p| !p.is_empty())
                            .map(str::to_string)
                            .collect(),
                        nargs: nargs.parse().ok()?,
                        body: private_aliases(&unescape(body), name),
                    },
                ))
            })
            .collect()
    })
}

/// The body with each derived-table alias it declares (`( SELECT ... ) ss`)
/// renamed for the helper, so a caller's argument naming its own `ss` is not
/// captured by the body's once spliced in. PostgreSQL inlines an analysed
/// tree, where names are already resolved; text has to keep them apart.
fn private_aliases(body: &str, helper: &str) -> String {
    let Ok(scan) = pg_query::scan(body) else {
        return body.to_string();
    };
    let tokens = scan.tokens;
    let aliases: Vec<String> = tokens
        .windows(2)
        .filter(|w| text_at(body, &w[0]) == ")" && is_name(&w[1]))
        .map(|w| ident(text_at(body, &w[1])))
        .filter(|a| !matches!(a.as_str(), "where" | "and" | "or" | "then" | "else" | "end"))
        .collect();
    if aliases.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;
    for t in &tokens {
        if is_name(t) && aliases.contains(&ident(text_at(body, t))) {
            out.push_str(&body[cursor..t.start as usize]);
            out.push_str(&format!("{}_{helper}", ident(text_at(body, t))));
            cursor = t.end as usize;
        }
    }
    out.push_str(&body[cursor..]);
    out
}

/// An identifier token's name, folded as the parser folds it.
fn ident(raw: &str) -> String {
    match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(q) => q.replace("\"\"", "\""),
        None => raw.to_ascii_lowercase(),
    }
}

fn is_name(t: &pg_query::protobuf::ScanToken) -> bool {
    t.token == Token::Ident as i32
        || t.keyword_kind == pg_query::protobuf::KeywordKind::UnreservedKeyword as i32
        || t.keyword_kind == pg_query::protobuf::KeywordKind::ColNameKeyword as i32
}

/// The body with each parameter replaced by its argument's text.
fn substitute(h: &Helper, args: &[String]) -> Option<String> {
    let body = &h.body;
    let tokens = pg_query::scan(body).ok()?.tokens;
    let text = |i: usize| &body[tokens[i].start as usize..tokens[i].end as usize];
    let param_of = |i: usize| -> Option<usize> {
        let t = &tokens[i];
        if t.token == Token::Param as i32 {
            return text(i)
                .strip_prefix('$')?
                .parse::<usize>()
                .ok()?
                .checked_sub(1);
        }
        // A named parameter, not a qualified name's part or a function.
        if !is_name(t) || (i > 0 && text(i - 1) == ".") {
            return None;
        }
        if tokens
            .get(i + 1)
            .is_some_and(|n| matches!(text_at(body, n), "(" | "."))
        {
            return None;
        }
        h.params.iter().position(|p| *p == ident(text(i)))
    };
    let mut out = String::new();
    let mut cursor = 0;
    let mut i = 0;
    while i < tokens.len() {
        let Some(n) = param_of(i) else {
            i += 1;
            continue;
        };
        let arg = args.get(n)?;
        let (start, mut end) = (tokens[i].start as usize, tokens[i].end as usize);
        let mut start = start;
        let mut replacement = format!("({arg})");
        // `($n).field` over `t.*`: the row's column.
        if let Some(row) = arg.strip_suffix(".*") {
            let open = i > 0 && text(i - 1) == "(";
            let close = tokens.get(i + 1).is_some_and(|t| text_at(body, t) == ")");
            let dot = tokens.get(i + 2).is_some_and(|t| text_at(body, t) == ".");
            if let (true, true, true, Some(field)) = (open, close, dot, tokens.get(i + 3)) {
                start = tokens[i - 1].start as usize;
                end = field.end as usize;
                replacement = format!("{row}.{}", text_at(body, field));
                i += 3;
            }
        }
        out.push_str(&body[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
        i += 1;
    }
    out.push_str(&body[cursor..]);
    Some(out)
}

fn text_at<'a>(s: &'a str, t: &pg_query::protobuf::ScanToken) -> &'a str {
    &s[t.start as usize..t.end as usize]
}

/// One pass: every `information_schema.<helper>(...)` call replaced.
fn inline_once(sql: &str) -> Option<String> {
    let tokens = pg_query::scan(sql).ok()?.tokens;
    let text = |i: usize| text_at(sql, &tokens[i]);
    let mut out = String::new();
    let mut cursor = 0;
    let mut changed = false;
    let mut i = 0;
    while i + 3 < tokens.len() {
        let call = is_name(&tokens[i])
            && ident(text(i)) == "information_schema"
            && text(i + 1) == "."
            && text(i + 3) == "(";
        let helper = call.then(|| helpers().get(&ident(text(i + 2)))).flatten();
        let Some(h) = helper else {
            i += 1;
            continue;
        };
        // The arguments, split at the call's own commas.
        let mut depth = 0usize;
        let mut args = Vec::new();
        let mut arg_start = tokens[i + 3].end as usize;
        let mut j = i + 3;
        let close = loop {
            j += 1;
            let t = tokens.get(j)?;
            match text_at(sql, t) {
                "(" | "[" => depth += 1,
                ")" if depth == 0 => {
                    args.push(sql[arg_start..t.start as usize].trim().to_string());
                    break j;
                }
                ")" | "]" => depth -= 1,
                "," if depth == 0 => {
                    args.push(sql[arg_start..t.start as usize].trim().to_string());
                    arg_start = t.end as usize;
                }
                _ => {}
            }
        };
        if args.len() != h.nargs {
            i += 1;
            continue;
        }
        let body = substitute(h, &args)?;
        out.push_str(&sql[cursor..tokens[i].start as usize]);
        out.push('(');
        out.push_str(&body);
        out.push(')');
        cursor = tokens[close].end as usize;
        changed = true;
        i = close + 1;
    }
    out.push_str(&sql[cursor..]);
    changed.then_some(out)
}

/// `sql` with every helper call inlined, or `None` when it calls none.
pub(crate) fn inline(sql: &str) -> Option<String> {
    if !sql.contains("_pg_") {
        return None;
    }
    let mut current = inline_once(sql)?;
    for _ in 0..8 {
        match inline_once(&current) {
            Some(next) => current = next,
            None => break,
        }
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_parameters_take_their_arguments() {
        let sql =
            inline("select information_schema._pg_numeric_scale(a.atttypid, a.atttypmod) from t a")
                .unwrap();
        assert!(sql.contains("((a.atttypid) = ANY"), "{sql}");
        assert!(!sql.contains("typid ="), "{sql}");
    }

    #[test]
    fn a_whole_row_argument_reads_its_column() {
        let sql = inline("select information_schema._pg_truetypid(a.*, t.*)").unwrap();
        assert!(sql.contains("t.typtype"), "{sql}");
        assert!(sql.contains("a.atttypid"), "{sql}");
        assert!(!sql.contains('$'), "{sql}");
    }

    #[test]
    fn a_body_alias_does_not_capture_an_argument() {
        let sql = inline(
            "select information_schema._pg_index_position(ss.conindid, ss.confkey[1]) from t ss",
        )
        .unwrap();
        assert!(sql.contains("ss__pg_index_position"), "{sql}");
        assert!(sql.contains("(ss.conindid)"), "{sql}");
        assert!(sql.contains("(ss.confkey[1])"), "{sql}");
    }

    #[test]
    fn nested_helpers_are_inlined() {
        let sql = inline("select information_schema._pg_char_octet_length(x, y)").unwrap();
        assert!(!sql.contains("_pg_char_max_length"), "{sql}");
    }
}
