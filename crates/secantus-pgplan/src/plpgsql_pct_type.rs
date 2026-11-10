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

/// The type of `relation.column` (`schema.relation.column`), with its
/// modifier, when the relation exists and has the column.
fn column_type(
    schema: Option<&String>,
    relation: &str,
    column: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let key = match schema {
        None => schemas::resolve_unqualified(relation),
        Some(s) if crate::is_pg_temp_schema(s) => {
            schemas::relation_key(&schemas::temp_schema()?, relation)
        }
        Some(s) => {
            if !schemas::schema_exists(s) {
                return None;
            }
            schemas::relation_key(s, relation)
        }
    };
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

#[cfg(test)]
mod tests {
    use super::plpgsql_percent_type_words as words;

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
