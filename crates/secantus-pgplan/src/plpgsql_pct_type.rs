//! PL/pgSQL's `%TYPE` outside the function's own namespace (RP-818).
//!
//! `read_datatype` hands `word%TYPE` to `plpgsql_parse_wordtype` and
//! `a.b%TYPE` / `a.b.c%TYPE` to `plpgsql_parse_cwordtype` (PostgreSQL 15's
//! `pl_comp.c`; libpg_query stubs both out). Each looks in the function's
//! namespace first -- a variable, or a block-qualified one -- which the
//! interpreter does (`plpgsql_fn::percent_types`); what is left is here:
//!
//! - a single word no variable has is looked up as a type name
//!   (`LookupTypeName`), and a shell or composite type is not one;
//! - two words are a relation and its column, three a schema, a relation
//!   and its column (`RangeVarGetRelid` with `missing_ok`, so a missing
//!   schema is not an error), and the column is any of its attributes, a
//!   system column included: its type and modifier (`atttypid`,
//!   `atttypmod`).
//!
//! Anything not found is `None`: `read_datatype` then parses the whole
//! text as a type name, which refuses the `%` as a syntax error.

use crate::{domains, pgtypes, scalar, schemas, user_base_type, user_enum, TableDef};

/// A `%TYPE` reference's text as PL/pgSQL's scanner reads it (`t.c%type`,
/// `"T".c %TYPE`, `$1%type`): its words, a quoted one as written and any
/// other folded to lower case. `None` when the text is not one or more
/// words, separated by dots, followed by `%TYPE` and nothing else.
pub fn plpgsql_percent_type_words(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    // The `%` outside any quoted word.
    let mut in_quote = false;
    let mut at = None;
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            '%' if !in_quote => {
                at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let at = at?;
    if !text[at + 1..].trim().eq_ignore_ascii_case("type") {
        return None;
    }
    let mut words = Vec::new();
    let mut rest = text[..at].trim();
    loop {
        let (word, after) = if let Some(quoted) = rest.strip_prefix('"') {
            // A doubled quote is one quote in the name.
            let mut name = String::new();
            let mut chars = quoted.char_indices().peekable();
            let mut end = None;
            while let Some((i, c)) = chars.next() {
                if c == '"' {
                    if chars.peek().is_some_and(|(_, n)| *n == '"') {
                        chars.next();
                        name.push('"');
                        continue;
                    }
                    end = Some(i + 1);
                    break;
                }
                name.push(c);
            }
            let end = end?;
            if name.is_empty() {
                return None;
            }
            (name, &quoted[end..])
        } else {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let starts_ok = word
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$');
            if !starts_ok {
                return None;
            }
            (word.to_lowercase(), &rest[end..])
        };
        words.push(word);
        let after = after.trim_start();
        if after.is_empty() {
            return Some(words);
        }
        rest = after.strip_prefix('.')?.trim_start();
    }
}

/// What `%TYPE` over `words` names when it is not one of the function's
/// variables (see the module comment), as a declared type's text (a
/// domain's or enum's name, a built-in with its modifier:
/// `character varying(3)`), or `None`.
pub fn plpgsql_percent_type_outer(
    words: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    match words {
        [word] => word_type(word),
        [relation, column] => column_type(None, relation, column, lookup),
        [schema, relation, column] => column_type(Some(schema), relation, column, lookup),
        _ => None,
    }
}

/// `plpgsql_parse_wordtype`'s fallback: a type named `word` on the search
/// path that is defined and not a composite (`typisdefined`, no
/// `typrelid`). The name is the type's own (`int4`, not `int` or
/// `integer`, which are the grammar's).
fn word_type(word: &str) -> Option<String> {
    if word.ends_with("[]") {
        return None;
    }
    let builtin = pgtypes::BUILTIN_TYPES.iter().any(|(t, _, _)| *t == word);
    let user = user_enum(word).is_some()
        || domains::user_domain(word).is_some()
        || user_base_type(word).is_some_and(|(_, _, defined)| defined);
    (builtin || user).then(|| word.to_string())
}

/// The catalog key of `relation` as `RangeVarGetRelid` finds it: on the
/// path when unqualified, else in `schema` -- `pg_temp` only once the
/// session has its temporary schema, a catalog by its bare name. `None`
/// when the schema does not exist (`LookupExplicitNamespace`).
fn relation_key_of(schema: Option<&str>, relation: &str) -> Option<String> {
    let Some(s) = schema else {
        return Some(schemas::resolve_unqualified(relation));
    };
    let resolved = if crate::is_pg_temp_schema(s) {
        schemas::temp_schema_in_use().filter(|t| s == "pg_temp" || t == s)?
    } else if schemas::schema_exists(s) {
        s.to_string()
    } else {
        return None;
    };
    Some(if resolved == "pg_catalog" {
        relation.to_string()
    } else {
        schemas::relation_key(&resolved, relation)
    })
}

/// The type of `relation.column` (`schema.relation.column`), with its
/// modifier, when the relation exists and has the column.
fn column_type(
    schema: Option<&String>,
    relation: &str,
    column: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let key = relation_key_of(schema.map(String::as_str), relation)?;
    let def = lookup(&key)?;
    let c = def.column(column)?;
    // A domain column is of the domain, which carries the modifier
    // (`atttypmod` -1); an enum column of its enum.
    for key in ["domain_type", "enum_type"] {
        if let Ok(t) = c.extra.get_str(key) {
            return Some(t.to_string());
        }
    }
    Some(if c.typmod >= 0 {
        scalar::format_type_text_public(&c.pg_type, Some(c.typmod))
    } else {
        c.pg_type.clone()
    })
}

/// `LookupTypeNameExtended`'s `%TYPE` branch, for a routine's parameter or
/// result type (RP-822): CREATE FUNCTION / PROCEDURE look each one up with
/// `missing_ok` false and no parse state, so every error here has no
/// position.
///
/// The words are `relation.column`, `schema.relation.column` or
/// `database.schema.relation.column` (the grammar refuses a lone word).
/// The relation is found as `RangeVarGetRelid` finds it -- 0A000 for
/// another database, 3F000 for a missing schema, 42P01 for a missing
/// relation -- and the column is any of its attributes, a system column
/// included, else 42703. The type is the column's, WITHOUT its modifier
/// (`atttypid` only), and a NOTICE says so (`type reference t.c%TYPE
/// converted to character varying`). The answer is a type name naming that
/// type, the `SETOF` and position kept, which the routine then stores as
/// if it had been written out.
pub(crate) fn routine_percent_type(
    tn: &pg_query::protobuf::TypeName,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> crate::Result<pg_query::protobuf::TypeName> {
    use crate::Error;
    use pg_query::protobuf::node::Node as N;
    let words: Vec<String> = tn
        .names
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect();
    let refuse = |e: Error| {
        crate::set_error_unpositioned();
        Err(e)
    };
    // `NameListToString`: the words joined by dots, never quoted.
    let written = words.join(".");
    let (schema, relation, column) = match words.as_slice() {
        [_] => {
            return refuse(Error::Sqlstate(
                "42601",
                format!("improper %TYPE reference (too few dotted names): {written}"),
            ))
        }
        [relation, column] => (None, relation, column),
        [schema, relation, column] => (Some(schema.clone()), relation, column),
        [catalog, schema, relation, column] => {
            if *catalog != crate::session_database() {
                return refuse(Error::Sqlstate(
                    "0A000",
                    format!(
                        "cross-database references are not implemented: \"{catalog}.{schema}.{relation}\""
                    ),
                ));
            }
            (Some(schema.clone()), relation, column)
        }
        _ => {
            return refuse(Error::Sqlstate(
                "42601",
                format!("improper %TYPE reference (too many dotted names): {written}"),
            ))
        }
    };
    let Some(key) = relation_key_of(schema.as_deref(), relation) else {
        return refuse(Error::Sqlstate(
            "3F000",
            format!("schema \"{}\" does not exist", schema.unwrap_or_default()),
        ));
    };
    let Some(def) = lookup(&key) else {
        let shown = match &schema {
            Some(s) => format!("{s}.{relation}"),
            None => relation.clone(),
        };
        // Not `UndefinedTable`, which the server places at the name.
        return refuse(Error::Sqlstate(
            "42P01",
            format!("relation \"{shown}\" does not exist"),
        ));
    };
    let Some(c) = def.column(column) else {
        return refuse(Error::Sqlstate(
            "42703",
            format!("column \"{column}\" of relation \"{relation}\" does not exist"),
        ));
    };
    // A domain column is of the domain, an enum column of its enum.
    let ty = ["domain_type", "enum_type"]
        .iter()
        .find_map(|k| c.extra.get_str(k).ok().map(str::to_string))
        .unwrap_or_else(|| c.pg_type.clone());
    // `format_type_be`: no modifier.
    crate::fts::planner_notice(&format!(
        "type reference {written}%TYPE converted to {}",
        scalar::format_type_text_public(&ty, None)
    ));
    let (element, array) = match ty.strip_suffix("[]") {
        Some(e) => (e, true),
        None => (ty.as_str(), false),
    };
    let string = |s: &str| pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String { sval: s.into() })),
    };
    // A schema-qualified type keeps its schema as its own name part.
    let names = match element.split_once('.') {
        Some((s, n)) if !element.starts_with('"') => vec![string(s), string(n)],
        _ => vec![string(element)],
    };
    let array_bounds = if array {
        vec![pg_query::protobuf::Node {
            node: Some(N::Integer(pg_query::protobuf::Integer { ival: -1 })),
        }]
    } else {
        Vec::new()
    };
    Ok(pg_query::protobuf::TypeName {
        names,
        type_oid: 0,
        setof: tn.setof,
        pct_type: false,
        typmods: Vec::new(),
        typemod: -1,
        array_bounds,
        location: tn.location,
    })
}

#[cfg(test)]
mod tests {
    use super::plpgsql_percent_type_words as words;

    /// The first parameter's type name of a CREATE FUNCTION.
    fn first_parameter_type(sql: &str) -> pg_query::protobuf::TypeName {
        use pg_query::protobuf::node::Node as N;
        let tree = pg_query::parse(sql).expect("parses");
        let Some(N::CreateFunctionStmt(f)) = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .and_then(|s| s.node.clone())
        else {
            panic!("not a CREATE FUNCTION")
        };
        match f.parameters[0].node.as_ref() {
            Some(N::FunctionParameter(p)) => p.arg_type.clone().expect("typed"),
            _ => panic!("no parameter"),
        }
    }

    #[test]
    fn a_routines_percent_type_nothing_answers_is_refused() {
        let refused = |sql: &str| {
            let tn = first_parameter_type(sql);
            assert!(tn.pct_type);
            super::routine_percent_type(&tn, &|_| None)
                .expect_err("refused")
                .to_string()
        };
        let body = " returns int language sql as $$ select 1 $$";
        assert!(
            refused(&format!("create function f(x a.b.c.d.e%type){body}"))
                .contains("improper %TYPE reference (too many dotted names): a.b.c.d.e")
        );
        assert!(refused(&format!("create function f(x nope.v%type){body}"))
            .contains("relation \"nope\" does not exist"));
        assert!(
            refused(&format!("create function f(x nos.nope.v%type){body}"))
                .contains("schema \"nos\" does not exist")
        );
        // No temporary table yet: `pg_temp` names no schema.
        assert!(
            refused(&format!("create function f(x pg_temp.t.v%type){body}"))
                .contains("schema \"pg_temp\" does not exist")
        );
    }

    fn w(parts: &[&str]) -> Option<Vec<String>> {
        Some(parts.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn reads_the_words_of_a_percent_type() {
        assert_eq!(words("t809.v%type"), w(&["t809", "v"]));
        assert_eq!(words("T809.V %TYPE "), w(&["t809", "v"]));
        assert_eq!(words("t809.v % type"), w(&["t809", "v"]));
        assert_eq!(words("\"T809\".v%type"), w(&["T809", "v"]));
        assert_eq!(words("\"a\"\"b\".c%type"), w(&["a\"b", "c"]));
        assert_eq!(words("public.t809.v%type"), w(&["public", "t809", "v"]));
        assert_eq!(words("$1%type"), w(&["$1"]));
        assert_eq!(words("a%type"), w(&["a"]));
        // A %ROWTYPE, an array of one, and plain type names are not.
        assert_eq!(words("t809%rowtype"), None);
        assert_eq!(words("t809.v%type[]"), None);
        assert_eq!(words("varchar(3)"), None);
        assert_eq!(words("int"), None);
        assert_eq!(words("%type"), None);
        assert_eq!(words("a..b%type"), None);
    }
}
