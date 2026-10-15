//! The text-search parser's set-returning functions: `ts_token_type`,
//! `ts_parse` and `ts_debug`, over the default parser (`wparser.rs`), the
//! only one PostgreSQL ships.

use crate::{fts, value_text, wparser, Error, Result};
use bson::Bson;

/// Output column names, their types, and the rows.
pub(crate) type Rows = (Vec<String>, Vec<String>, Vec<Vec<Bson>>);

fn cols(names: &[&str], types: &[&str]) -> (Vec<String>, Vec<String>) {
    (
        names.iter().map(|s| s.to_string()).collect(),
        types.iter().map(|s| s.to_string()).collect(),
    )
}

/// Resolve a parser argument, named (`default`, `pg_catalog.default`) or by
/// oid (3722). `false` for NULL: the functions are strict.
fn parser_arg(arg: &Bson) -> Result<bool> {
    match arg {
        Bson::Null => Ok(false),
        Bson::Int32(_) | Bson::Int64(_) => {
            let oid = match arg {
                Bson::Int32(i) => i64::from(*i),
                Bson::Int64(i) => *i,
                _ => 0,
            };
            if oid != wparser::DEFAULT_PARSER_OID {
                return Err(Error::Internal(format!(
                    "cache lookup failed for text search parser {oid}"
                )));
            }
            Ok(true)
        }
        // A name is a qualified identifier list (`stringToQualifiedNameList`):
        // an unquoted part folds to lower case, so `Default` is the parser.
        other => {
            let parts = crate::split_relation_name(&value_text(other))?;
            let joined = parts.join(".");
            let (schema, name) = match parts.as_slice() {
                [n] => (None, n),
                [s, n] => (Some(s), n),
                [_, _, _] => {
                    return Err(Error::FeatureNotSupported(format!(
                        "cross-database references are not implemented: {joined}"
                    )))
                }
                _ => {
                    return Err(Error::Sqlstate(
                        "42601",
                        format!("improper qualified name (too many dotted names): {joined}"),
                    ))
                }
            };
            if let Some(s) = schema {
                if !crate::schemas::schema_exists(s) {
                    return Err(Error::Sqlstate(
                        "3F000",
                        format!("schema \"{s}\" does not exist"),
                    ));
                }
            }
            if name != "default" || schema.is_some_and(|s| s != "pg_catalog") {
                return Err(Error::Sqlstate(
                    "42704",
                    format!("text search parser \"{joined}\" does not exist"),
                ));
            }
            Ok(true)
        }
    }
}

/// `ts_token_type(parser)`: the default parser's 23 token types
/// (`wparser_def.c`'s `tok_alias` / `lex_descr`).
pub(crate) fn token_type(args: &[Bson]) -> Result<Rows> {
    let (names, types) = cols(
        &["tokid", "alias", "description"],
        &["int4", "text", "text"],
    );
    if !parser_arg(&args[0])? {
        return Ok((names, types, Vec::new()));
    }
    let rows = wparser::TOKEN_TYPES
        .iter()
        .enumerate()
        .map(|(i, (alias, descr))| {
            vec![
                Bson::Int32(i as i32 + 1),
                Bson::String((*alias).into()),
                Bson::String((*descr).into()),
            ]
        })
        .collect();
    Ok((names, types, rows))
}

/// `ts_parse(parser, document)`: every token, blanks included, with its
/// type id.
pub(crate) fn parse(args: &[Bson]) -> Result<Rows> {
    let (names, types) = cols(&["tokid", "token"], &["int4", "text"]);
    // The parser is resolved first: a bad one is an error even over NULL.
    if !parser_arg(&args[0])? || args[1] == Bson::Null {
        return Ok((names, types, Vec::new()));
    }
    let rows = wparser::parse(&value_text(&args[1]))
        .into_iter()
        .map(|(ty, tok)| vec![Bson::Int32(i32::from(ty)), Bson::String(tok)])
        .collect();
    Ok((names, types, rows))
}

/// `ts_debug([config,] document)`: PostgreSQL's SQL function, which joins
/// `ts_parse` to `ts_token_type` and asks each token's dictionaries for
/// its lexemes. A type the configuration maps to no dictionary (a blank, a
/// tag) has `{}` dictionaries and NULL for the rest; the session default
/// configuration is `english`.
pub(crate) fn debug(args: &[Bson]) -> Result<Rows> {
    let (names, types) = cols(
        &[
            "alias",
            "description",
            "token",
            "dictionaries",
            "dictionary",
            "lexemes",
        ],
        &[
            "text",
            "text",
            "text",
            "regdictionary[]",
            "regdictionary",
            "text[]",
        ],
    );
    let (cfg, doc) = match args {
        [doc] => (None, doc),
        [cfg, doc] => (Some(cfg), doc),
        _ => return Err(Error::Parse("function ts_debug does not exist".into())),
    };
    let cfg = match cfg {
        Some(Bson::Null) => return Ok((names, types, Vec::new())),
        Some(c) => fts::config(&value_text(c))?,
        None => fts::default_config(),
    };
    if *doc == Bson::Null {
        return Ok((names, types, Vec::new()));
    }
    let rows = wparser::parse(&value_text(doc))
        .into_iter()
        .map(|(ty, tok)| {
            let (alias, descr) = wparser::TOKEN_TYPES[usize::from(ty) - 1];
            let (dicts, dict, lexemes) = match fts::debug_lexize(cfg, ty, &tok) {
                Some((dict, lexemes)) => (
                    Bson::Array(vec![Bson::String(dict.clone())]),
                    Bson::String(dict),
                    Bson::Array(lexemes.into_iter().map(Bson::String).collect()),
                ),
                None => (Bson::Array(Vec::new()), Bson::Null, Bson::Null),
            };
            vec![
                Bson::String(alias.into()),
                Bson::String(descr.into()),
                Bson::String(tok),
                dicts,
                dict,
                lexemes,
            ]
        })
        .collect();
    Ok((names, types, rows))
}
