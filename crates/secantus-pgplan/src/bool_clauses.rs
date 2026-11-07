//! An untyped string literal where PostgreSQL wants a boolean -- a WHERE,
//! a HAVING, a JOIN's ON, an argument of AND / OR / NOT, a searched CASE's
//! WHEN, an aggregate's FILTER -- is of type UNKNOWN, and
//! `coerce_to_boolean` coerces it with `coerce_type`, which reads an
//! unknown Const through the type's input function at parse analysis: the
//! clause becomes the boolean constant `boolin` makes of the text. `where
//! 't'` keeps every row and `where 'off'` none (RP-424).
//!
//! A literal that reads as a boolean is replaced here by that constant, the
//! node `true` / `false` parse to, so every later stage sees what
//! PostgreSQL's analysed query holds. One that does not read is left as it
//! is, for [`check`] to refuse where parse analysis reaches it: 22P02
//! `invalid input syntax for type boolean` at the literal. Before this the
//! literal stayed a string and was judged by its Rust value: 42804 `not
//! type text` in a WHERE, and no rows at all in an ON or a CASE WHEN.
//! Measured against PostgreSQL 15.19.

use super::*;
use pg_query::protobuf::a_const::Val;

/// PostgreSQL's `boolin`: the text with its surrounding blanks gone, read by
/// `parse_bool_with_len` -- any case-blind prefix of `true`, `false`, `yes`
/// or `no`, `on`, `off` or its prefix `of`, and `1` / `0` alone.
pub(crate) fn bool_in(text: &str) -> Option<bool> {
    // `boolin` trims with `isspace`, which in the C locale is these six.
    let blank = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c');
    let s = text.trim_matches(blank).to_ascii_lowercase();
    let prefix_of = |word: &str| !s.is_empty() && word.starts_with(s.as_str());
    match s.chars().next()? {
        't' if prefix_of("true") => Some(true),
        'f' if prefix_of("false") => Some(false),
        'y' if prefix_of("yes") => Some(true),
        'n' if prefix_of("no") => Some(false),
        // `pg_strncasecmp(value, "on", Max(len, 2))`: at least two
        // characters are compared, so a lone `o` is neither.
        'o' if s.len() >= 2 && prefix_of("on") => Some(true),
        'o' if s.len() >= 2 && prefix_of("off") => Some(false),
        '1' if s.len() == 1 => Some(true),
        '0' if s.len() == 1 => Some(false),
        _ => None,
    }
}

/// The untyped string literal `n` is, by its text and location.
fn unknown_literal(n: &pg_query::protobuf::Node) -> Option<(&str, i32)> {
    match n.node.as_ref()? {
        N::AConst(c) if !c.isnull => match c.val.as_ref()? {
            Val::Sval(s) => Some((s.sval.as_str(), c.location)),
            _ => None,
        },
        _ => None,
    }
}

/// `n` coerced to boolean: a literal that reads as one becomes its
/// constant; anything else is left for analysis to judge.
fn coerce(n: &mut pg_query::protobuf::Node) {
    let Some((text, location)) = unknown_literal(n) else {
        return;
    };
    let Some(value) = bool_in(text) else {
        return;
    };
    n.node = Some(N::AConst(pg_query::protobuf::AConst {
        isnull: false,
        location,
        val: Some(Val::Boolval(pg_query::protobuf::Boolean { boolval: value })),
    }));
}

fn coerce_boxed(n: &mut Option<Box<pg_query::protobuf::Node>>) {
    if let Some(n) = n.as_deref_mut() {
        coerce(n);
    }
}

/// Replace every untyped literal in a boolean position that reads as a
/// boolean by that boolean.
pub(crate) fn rewrite(node: &mut N) {
    let wanted = node.nodes().iter().any(|(n, _, _, _)| {
        let any = |c: &Option<Box<pg_query::protobuf::Node>>| {
            c.as_deref().and_then(unknown_literal).is_some()
        };
        match n {
            pg_query::NodeRef::SelectStmt(s) => any(&s.where_clause) || any(&s.having_clause),
            pg_query::NodeRef::UpdateStmt(s) => any(&s.where_clause),
            pg_query::NodeRef::DeleteStmt(s) => any(&s.where_clause),
            pg_query::NodeRef::JoinExpr(j) => any(&j.quals),
            pg_query::NodeRef::BoolExpr(b) => b.args.iter().any(|a| unknown_literal(a).is_some()),
            pg_query::NodeRef::CaseWhen(w) => any(&w.expr),
            pg_query::NodeRef::FuncCall(f) => any(&f.agg_filter),
            _ => false,
        }
    });
    if !wanted {
        return;
    }
    // SAFETY: the pointers `nodes_mut` hands out point into `node`, which
    // outlives the loop; each clause is changed in place and no node the
    // walk listed is freed (a literal has no children).
    unsafe {
        for (n, _, _) in node.nodes_mut() {
            match n {
                pg_query::NodeMut::SelectStmt(s) => {
                    let s = &mut *s;
                    coerce_boxed(&mut s.where_clause);
                    coerce_boxed(&mut s.having_clause);
                }
                pg_query::NodeMut::UpdateStmt(s) => coerce_boxed(&mut (*s).where_clause),
                pg_query::NodeMut::DeleteStmt(s) => coerce_boxed(&mut (*s).where_clause),
                pg_query::NodeMut::JoinExpr(j) => coerce_boxed(&mut (*j).quals),
                pg_query::NodeMut::BoolExpr(b) => (*b).args.iter_mut().for_each(coerce),
                pg_query::NodeMut::CaseExpr(c) => {
                    let c = &mut *c;
                    // A simple CASE compares its WHENs to its argument; only
                    // a searched one reads each WHEN as a condition.
                    if c.arg.is_some() {
                        continue;
                    }
                    for w in &mut c.args {
                        if let Some(N::CaseWhen(w)) = w.node.as_mut() {
                            coerce_boxed(&mut w.expr);
                        }
                    }
                }
                pg_query::NodeMut::FuncCall(f) => coerce_boxed(&mut (*f).agg_filter),
                _ => {}
            }
        }
    }
}

/// A boolean position's untyped literal that [`rewrite`] left, because its
/// text is no boolean: `boolin`'s 22P02 at the literal, where parse
/// analysis coerces the clause.
pub(crate) fn check(n: &pg_query::protobuf::Node) -> Result<()> {
    let Some((text, location)) = unknown_literal(n) else {
        return Ok(());
    };
    if bool_in(text).is_some() {
        return Ok(());
    }
    if location >= 0 {
        set_error_location(location);
    }
    Err(Error::InvalidText(format!(
        "invalid input syntax for type boolean: \"{text}\""
    )))
}

#[cfg(test)]
mod tests {
    use super::bool_in;

    /// `parse_bool_with_len`'s readings, each measured on PostgreSQL 15.19
    /// as `select '<text>'::bool`.
    #[test]
    fn reads_as_boolin_does() {
        for (text, want) in [
            ("t", Some(true)),
            ("tr", Some(true)),
            ("TRU", Some(true)),
            ("true", Some(true)),
            ("truex", None),
            ("f", Some(false)),
            ("fal", Some(false)),
            ("y", Some(true)),
            ("ye", Some(true)),
            ("n", Some(false)),
            ("no", Some(false)),
            ("o", None),
            ("on", Some(true)),
            ("of", Some(false)),
            ("off", Some(false)),
            ("off ", Some(false)),
            (" t ", Some(true)),
            ("\tt", Some(true)),
            ("1", Some(true)),
            ("0", Some(false)),
            ("10", None),
            ("", None),
            ("x", None),
        ] {
            assert_eq!(bool_in(text), want, "{text:?}");
        }
    }
}
