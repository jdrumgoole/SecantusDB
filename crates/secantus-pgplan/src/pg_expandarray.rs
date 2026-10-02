//! `information_schema._pg_expandarray(arr)` in a select list: PostgreSQL's
//! SQL function returning one `(x, n)` record per array element, `n`
//! counting from 1. pgjdbc's `getPrimaryKeys` (and so every updatable
//! result set) is built on it:
//!
//! ```sql
//! SELECT (information_schema._pg_expandarray(i.indkey)).n AS key_seq,
//!        information_schema._pg_expandarray(i.indkey) AS keys, ...
//! ```
//!
//! Every call over the SAME argument runs in lockstep (one row per element),
//! so each becomes a column of one LATERAL `unnest(arr) WITH ORDINALITY
//! __pgx(x, n)`: `(f(arr)).x` is `__pgx.x`, and the bare call is the named
//! record `ROW(__pgx.x, __pgx.n)`, whose `.x` / `.n` resolve by name.
//! (`FROM _pg_expandarray(arr)` is answered by `srf_rows`.)

use super::*;

fn is_expandarray(n: &pg_query::protobuf::Node) -> Option<&pg_query::protobuf::Node> {
    match n.node.as_ref() {
        Some(N::FuncCall(f))
            if f.over.is_none()
                && f.args.len() == 1
                && func_name(f).as_deref() == Some("_pg_expandarray") =>
        {
            f.args.first()
        }
        _ => None,
    }
}

/// The argument with its source locations blanked, so two calls over the
/// same expression at different places in the text compare equal.
fn arg_key(n: &pg_query::protobuf::Node) -> String {
    let text = format!("{n:?}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find("location: ") {
        out.push_str(&rest[..i]);
        rest = &rest[i + "location: ".len()..];
        rest = rest.trim_start_matches(|c: char| c == '-' || c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

fn column(field: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node("__pgx"), string_node(field)],
            location: -1,
        })),
    }
}

/// The select with its `_pg_expandarray` targets made a LATERAL join, or
/// `None` when it has none (or calls over different arguments, which keep
/// the refusal rather than a guessed lockstep).
pub(crate) fn rewrite(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let mut out = s.clone();
    let mut arg: Option<(String, pg_query::protobuf::Node)> = None;
    let mut mixed = false;
    let mut visit = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        // `(f(arr)).x` -> `__pgx.x`.
        let field_sel = match n.node.as_ref() {
            Some(N::AIndirection(ind)) => match (ind.arg.as_deref(), ind.indirection.as_slice()) {
                (Some(a), [field]) => match (is_expandarray(a), field.node.as_ref()) {
                    (Some(x), Some(N::String(f))) if f.sval == "x" || f.sval == "n" => {
                        Some((x.clone(), f.sval.clone()))
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        let (found, replacement) = if let Some((x, f)) = field_sel {
            (x, column(&f))
        } else if let Some(x) = is_expandarray(n) {
            let row = pg_query::protobuf::Node {
                node: Some(N::RowExpr(Box::new(pg_query::protobuf::RowExpr {
                    args: vec![column("x"), column("n")],
                    row_format: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                    colnames: vec![string_node("x"), string_node("n")],
                    location: -1,
                    ..Default::default()
                }))),
            };
            (x.clone(), row)
        } else {
            return Ok(());
        };
        let key = arg_key(&found);
        match &arg {
            Some((k, _)) if *k != key => mixed = true,
            Some(_) => {}
            None => arg = Some((key, found)),
        }
        *n = replacement;
        Ok(())
    };
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                // The output column keeps PostgreSQL's name for the call.
                let name = match v.node.as_ref() {
                    Some(N::AIndirection(ind)) => match ind.indirection.as_slice() {
                        [f] => match f.node.as_ref() {
                            Some(N::String(s))
                                if ind
                                    .arg
                                    .as_deref()
                                    .is_some_and(|a| is_expandarray(a).is_some()) =>
                            {
                                Some(s.sval.clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    },
                    _ if is_expandarray(v).is_some() => Some("_pg_expandarray".to_string()),
                    _ => None,
                };
                walk_expr(v, &mut visit)?;
                if rt.name.is_empty() {
                    if let Some(name) = name {
                        rt.name = name;
                    }
                }
            }
        }
    }
    let Some((_, arg)) = arg else {
        return Ok(None);
    };
    if mixed {
        return Err(Error::Unsupported(
            "_pg_expandarray() over different arrays in one select list".into(),
        ));
    }
    let mut parsed = pg_query::parse("SELECT FROM unnest(NULL) WITH ORDINALITY __pgx(x, n)")
        .map_err(|e| Error::Parse(e.to_string()))?
        .protobuf;
    let Some(N::SelectStmt(sel)) = parsed.stmts.pop().and_then(|r| r.stmt).and_then(|s| s.node)
    else {
        return Err(Error::Parse("_pg_expandarray rewrite".into()));
    };
    let mut item = sel.from_clause.into_iter().next().expect("one FROM item");
    if let Some(N::RangeFunction(rf)) = item.node.as_mut() {
        rf.lateral = !out.from_clause.is_empty();
        if let Some(N::List(l)) = rf.functions.first_mut().and_then(|f| f.node.as_mut()) {
            if let Some(N::FuncCall(f)) = l.items.first_mut().and_then(|c| c.node.as_mut()) {
                f.args = vec![arg];
            }
        }
    }
    out.from_clause.push(item);
    Ok(Some(out))
}
