//! `%TYPE` in a DECLARE section (RP-818): `k t.c%type`, `k a%type`,
//! `k blk.a%type`. PostgreSQL resolves it while it compiles the
//! declaration (`read_datatype`), against the namespace as it stands at
//! that point: `plpgsql_parse_wordtype` takes the type of a variable the
//! word names -- one declared earlier in the same block, in an enclosing
//! block, or the function's own (a parameter, by name or `$n`) -- and
//! `plpgsql_parse_cwordtype` the type of a block-qualified variable
//! (`label.name`, the function's name labelling its parameters). A word
//! naming a record or row variable is no type. What the namespace does not
//! answer is the catalog's: a type name, or a relation's column
//! (`secantus_pgplan::plpgsql_percent_type_outer`). The variable then has
//! that type and modifier, as if written out: `t.c%type` over a
//! `varchar(3)` column is `character varying(3)`.
//!
//! libpg_query stubs both lookups out and keeps the text as written, so
//! the declarations are resolved here, in the order the compiler makes
//! them, against the levels `block_scope` rebuilt.

use super::*;

/// A datum as a `%TYPE` lookup sees it: a variable and its type, or
/// something that has no scalar type (a record, a row, a field).
#[derive(Debug, Clone)]
pub(super) enum Kind {
    Var(String),
    Other,
}

/// The kinds of a parsed function's datums, a variable's type as the
/// interpreter spells it.
fn kinds(f: &Value) -> Vec<Kind> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .map(|d| match d.get("PLpgSQL_var") {
                    Some(v) => Kind::Var(
                        v.get("datatype")
                            .and_then(|t| t.get("PLpgSQL_type"))
                            .and_then(|t| t.get("typname"))
                            .and_then(Value::as_str)
                            .map(canonical_type)
                            .unwrap_or_else(|| "text".into()),
                    ),
                    None => Kind::Other,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The text of each `%TYPE` declaration, by datum, as written.
fn references(f: &Value) -> Vec<(usize, String)> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    let v = d.get("PLpgSQL_var")?;
                    v.get("lineno")?;
                    let text = v
                        .get("datatype")?
                        .get("PLpgSQL_type")?
                        .get("typname")?
                        .as_str()?;
                    secantus_pgplan::plpgsql_percent_type_words(text)?;
                    Some((i, text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A namespace level as a lookup sees it: its label, and its datums by
/// name.
type Level<'a> = (Option<&'a str>, Vec<(&'a str, usize)>);

/// The datum's name, a variable's or a record's.
fn datum_name(d: &Value) -> Option<&str> {
    d.get("PLpgSQL_var")
        .or_else(|| d.get("PLpgSQL_rec"))
        .and_then(|v| v.get("refname"))
        .and_then(Value::as_str)
}

/// Every `%TYPE` declaration of a parsed function, in datum order, with
/// the type it resolves to (`None`: nothing answers it, which PostgreSQL
/// reports as the type name's syntax error). `kinds` are the datums as
/// the caller knows them -- a running call's parameters have their
/// declared types -- and are updated as each declaration resolves, since
/// a later one may name it. `fname` is the function's name, the label of
/// its parameters; `outer` answers what the namespace does not.
pub(super) fn resolve(
    f: &Value,
    fname: Option<&str>,
    kinds: &mut [Kind],
    outer: &dyn Fn(&[String]) -> Option<String>,
) -> Vec<(usize, Option<String>)> {
    let refs = references(f);
    if refs.is_empty() {
        return Vec::new();
    }
    let datums: Vec<Value> = f
        .get("datums")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let names: Vec<Option<&str>> = datums.iter().map(datum_name).collect();
    // The function's own level: the parameters, FOUND and a trigger's
    // NEW / OLD, made before the body.
    let found = names.iter().position(|n| *n == Some("found"));
    let function_end = ["new_varno", "old_varno"]
        .iter()
        .filter_map(|k| f.get(*k).and_then(Value::as_u64))
        .map(|n| n as usize + 1)
        .fold(found.map_or(0, |i| i + 1), usize::max);
    // `$n`: the n-th variable or record before FOUND.
    let args: Vec<usize> = (0..found.unwrap_or(0))
        .filter(|&i| names[i].is_some())
        .collect();
    let scopes = block_scope::scopes(f);
    let in_function = |name: &str| -> Option<usize> {
        if let Some(n) = name.strip_prefix('$').and_then(|n| n.parse::<usize>().ok()) {
            return n.checked_sub(1).and_then(|i| args.get(i)).copied();
        }
        (0..function_end.min(names.len()))
            .rev()
            .find(|&i| names[i] == Some(name))
    };
    let mut out = Vec::new();
    for (d, text) in refs {
        let Some(words) = secantus_pgplan::plpgsql_percent_type_words(&text) else {
            continue;
        };
        // The level that declares `d`, and its declarations before `d`.
        let level = scopes.iter().enumerate().find_map(|(k, s)| {
            let at = s.vars.iter().position(|(_, v)| *v == d)?;
            Some((k, at))
        });
        // The levels `d` sees, innermost first, each with its label and
        // the datums it holds so far.
        let mut visible: Vec<Level<'_>> = Vec::new();
        match level {
            Some((k, at)) => {
                let s = &scopes[k];
                visible.push((
                    s.label.as_deref(),
                    s.vars[..at].iter().map(|(n, v)| (n.as_str(), *v)).collect(),
                ));
                let mut up = s.parent;
                while let Some(p) = up {
                    let s = &scopes[p];
                    visible.push((
                        s.label.as_deref(),
                        s.vars.iter().map(|(n, v)| (n.as_str(), *v)).collect(),
                    ));
                    up = s.parent;
                }
            }
            // No rebuilt levels (a body that does not scan): everything
            // the body made before it.
            None => visible.push((
                None,
                (function_end..d)
                    .filter_map(|i| Some((names[i]?, i)))
                    .collect(),
            )),
        }
        let find = |vars: &[(&str, usize)], name: &str| {
            vars.iter().rev().find(|(n, _)| *n == name).map(|(_, v)| *v)
        };
        let var_type = |datum: usize| match kinds.get(datum) {
            Some(Kind::Var(t)) => Some(t.clone()),
            _ => None,
        };
        let resolved = match words.as_slice() {
            [word] => {
                let hit = visible
                    .iter()
                    .find_map(|(_, vars)| find(vars, word))
                    .or_else(|| in_function(word));
                match hit {
                    // A record or a row is not a type here.
                    Some(datum) => var_type(datum),
                    None => outer(&words),
                }
            }
            [label, name] => {
                let hit = visible
                    .iter()
                    .filter(|(l, _)| *l == Some(label.as_str()))
                    .find_map(|(_, vars)| find(vars, name))
                    .or_else(|| {
                        (fname == Some(label.as_str()))
                            .then(|| in_function(name))
                            .flatten()
                    });
                // Not a variable: the first word may be a relation.
                hit.and_then(var_type).or_else(|| outer(&words))
            }
            _ => outer(&words),
        };
        if let (Some(t), Some(k)) = (&resolved, kinds.get_mut(d)) {
            *k = Kind::Var(canonical_type(t));
        }
        out.push((d, resolved.map(|t| canonical_type(&t))));
    }
    out
}

/// [`resolve`] over a CREATE FUNCTION or DO block's text: what CREATE
/// FUNCTION and a DO block check before the body runs. Empty when the body
/// does not parse.
///
/// A parameter's type is not in the parsed body when libpg_query could
/// not read it (it makes such a parameter a record), and is the call's at
/// run time; here a parameter the reference names answers it.
pub fn percent_types(
    create_sql: &str,
    outer: &dyn Fn(&[String]) -> Option<String>,
) -> Vec<(usize, Option<String>)> {
    let Ok(f) = parsed(create_sql) else {
        return Vec::new();
    };
    let mut kinds = kinds(&f);
    let names: Vec<Option<&str>> = f
        .get("datums")
        .and_then(Value::as_array)
        .map(|ds| ds.iter().map(datum_name).collect())
        .unwrap_or_default();
    if let Some(found) = names.iter().position(|n| *n == Some("found")) {
        for k in kinds.iter_mut().take(found) {
            if matches!(k, Kind::Other) {
                *k = Kind::Var(String::new());
            }
        }
    }
    resolve(&f, function_name(create_sql).as_deref(), &mut kinds, outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `%TYPE` declaration's name and what it resolves to, the
    /// catalog answering `t.c` alone (as `varchar(3)`).
    fn resolved(sql: &str) -> Vec<(String, Option<String>)> {
        // A DO block runs as this function (`do_block`).
        let create_sql = match sql.strip_prefix("do $$").and_then(|b| b.strip_suffix("$$")) {
            Some(body) => format!(
                "CREATE FUNCTION inline_code_block() RETURNS void AS \
                 $secantus_do${body}\n$secantus_do$ LANGUAGE plpgsql"
            ),
            None => sql.to_string(),
        };
        let create_sql = create_sql.as_str();
        let f = parsed(create_sql).unwrap();
        let names: Vec<String> = f["datums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| datum_name(d).unwrap_or_default().to_string())
            .collect();
        percent_types(create_sql, &|w| {
            (w == ["t".to_string(), "c".to_string()]).then(|| "varchar(3)".to_string())
        })
        .into_iter()
        .map(|(d, t)| (names[d].clone(), t))
        .collect()
    }

    fn r(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        pairs
            .iter()
            .map(|(n, t)| (n.to_string(), t.map(str::to_string)))
            .collect()
    }

    #[test]
    fn a_variable_takes_the_type_of_what_it_names() {
        assert_eq!(
            resolved("do $$ declare a int; k a%type; j k%type; begin end $$"),
            r(&[("k", Some("int4")), ("j", Some("int4"))])
        );
        assert_eq!(
            resolved("do $$ declare k t.c%type; m public.t.c%type; begin end $$"),
            r(&[("k", Some("varchar(3)")), ("m", None)])
        );
        // A record is no type, and the catalog is not asked.
        assert_eq!(
            resolved("do $$ declare c int; r record; k r%type; begin end $$"),
            r(&[("k", None)])
        );
    }

    #[test]
    fn a_variable_is_looked_up_where_it_is_declared() {
        // The innermost declaration so far, not a later one.
        assert_eq!(
            resolved("do $$ declare a int; begin declare a text; k a%type; begin end; end $$"),
            r(&[("k", Some("text"))])
        );
        assert_eq!(
            resolved("do $$ declare a int; begin declare k a%type; a text; begin end; end $$"),
            r(&[("k", Some("int4"))])
        );
        // A block's label qualifies its variables.
        assert_eq!(
            resolved(
                "do $$ <<b>> declare a text; begin declare a int; k b.a%type; begin end; end $$"
            ),
            r(&[("k", Some("text"))])
        );
    }

    #[test]
    fn a_parameter_is_named_by_name_by_number_and_under_the_function() {
        assert_eq!(
            resolved(
                "create function h(p int) returns int language plpgsql as $$ \
                 declare k p%type; j h.p%type; m $1%type; begin return 1; end $$"
            ),
            r(&[
                ("k", Some("int4")),
                ("j", Some("int4")),
                ("m", Some("int4"))
            ])
        );
    }
}
