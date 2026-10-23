//! `CREATE / ALTER / DROP TEXT SEARCH DICTIONARY` (RP-169), and what a
//! dictionary does with a token, as PostgreSQL 15.19's templates do it
//! (`dict_simple.c`, `dict_snowball.c`, `dict_synonym.c`): its options
//! checked as the template's `init` checks them, and its `lexize`. The
//! executor keeps the `pg_ts_dict` rows (`secantus-pg`'s `ts_dicts.rs`);
//! `ts_catalog` serves them, and `ts_lexize`, `to_tsvector` and the rest
//! lexize through them.
//!
//! The `ispell` and `thesaurus` templates are refused by name: their
//! dictionary files' formats are not read here.

use super::*;
use crate::ts_configs::QualifiedName;

/// A text-search dictionary statement.
#[derive(Debug, Clone, PartialEq)]
pub enum TsDictOp {
    /// `CREATE TEXT SEARCH DICTIONARY name (option = value, ...)`: the
    /// options as written, `template` among them.
    Create {
        name: QualifiedName,
        options: Vec<(String, Option<String>)>,
    },
    /// `ALTER TEXT SEARCH DICTIONARY name (option [= value], ...)`.
    Alter {
        name: QualifiedName,
        options: Vec<(String, Option<String>)>,
    },
    Drop {
        names: Vec<QualifiedName>,
        if_exists: bool,
        cascade: bool,
    },
    Rename {
        name: QualifiedName,
        to: String,
    },
    Owner {
        name: QualifiedName,
        owner: String,
    },
    SetSchema {
        name: QualifiedName,
        schema: String,
    },
}

/// The strings of a name node.
fn parts(n: &pg_query::protobuf::Node) -> QualifiedName {
    match n.node.as_ref() {
        Some(N::String(s)) => vec![s.sval.clone()],
        Some(N::List(l)) => l.items.iter().flat_map(parts).collect(),
        Some(N::TypeName(t)) => t.names.iter().flat_map(parts).collect(),
        _ => Vec::new(),
    }
}

/// A definition's options: each name, and its value as text (`None` with
/// none, as `ALTER ... (accept)` writes it).
fn options(defs: &[pg_query::protobuf::Node]) -> Vec<(String, Option<String>)> {
    defs.iter()
        .filter_map(|e| match e.node.as_ref() {
            Some(N::DefElem(e)) => Some((e.defname.clone(), e.arg.as_deref().map(def_text))),
            _ => None,
        })
        .collect()
}

/// `defGetString`: a value's text, a name's parts dotted.
fn def_text(n: &pg_query::protobuf::Node) -> String {
    match n.node.as_ref() {
        Some(N::String(s)) => s.sval.clone(),
        Some(N::Integer(i)) => i.ival.to_string(),
        Some(N::Float(f)) => f.fval.clone(),
        Some(N::Boolean(b)) => b.boolval.to_string(),
        Some(N::AConst(c)) => match c.val.as_ref() {
            Some(pg_query::protobuf::a_const::Val::Sval(s)) => s.sval.clone(),
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => i.ival.to_string(),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => f.fval.clone(),
            Some(pg_query::protobuf::a_const::Val::Boolval(b)) => b.boolval.to_string(),
            _ => String::new(),
        },
        _ => parts(n).join("."),
    }
}

/// `CREATE TEXT SEARCH DICTIONARY name (...)`.
pub(crate) fn plan_create(d: &pg_query::protobuf::DefineStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::Create {
        name: d.defnames.iter().flat_map(parts).collect(),
        options: options(&d.definition),
    }))
}

/// `ALTER TEXT SEARCH DICTIONARY name (...)`.
pub(crate) fn plan_alter(a: &pg_query::protobuf::AlterTsDictionaryStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::Alter {
        name: a.dictname.iter().flat_map(parts).collect(),
        options: options(&a.options),
    }))
}

/// `DROP TEXT SEARCH DICTIONARY [IF EXISTS] names [CASCADE]`.
pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::Drop {
        names: d.objects.iter().map(parts).collect(),
        if_exists: d.missing_ok,
        cascade: pg_query::protobuf::DropBehavior::try_from(d.behavior)
            == Ok(pg_query::protobuf::DropBehavior::DropCascade),
    }))
}

pub(crate) fn plan_rename(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::Rename {
        name: r.object.as_deref().map(parts).unwrap_or_default(),
        to: r.newname.clone(),
    }))
}

pub(crate) fn plan_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::Owner {
        name: a.object.as_deref().map(parts).unwrap_or_default(),
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}

pub(crate) fn plan_set_schema(a: &pg_query::protobuf::AlterObjectSchemaStmt) -> Result<Statement> {
    Ok(Statement::TsDict(TsDictOp::SetSchema {
        name: a.object.as_deref().map(parts).unwrap_or_default(),
        schema: a.newschema.clone(),
    }))
}

/// `serialize_deflist`: `dictinitoption`'s text, `name = 'value'` each.
pub fn options_text(options: &[(String, String)]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    Some(
        options
            .iter()
            .map(|(k, v)| format!("{k} = '{}'", v.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// `deserialize_deflist` for the dumped rows' option text.
pub fn parse_options_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let after = after.trim_start();
        let Some(body) = after.strip_prefix('\'') else {
            break;
        };
        let mut value = String::new();
        let mut chars = body.char_indices().peekable();
        let mut end = body.len();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().map(|(_, n)| *n) == Some('\'') {
                    value.push('\'');
                    chars.next();
                    continue;
                }
                end = i + 1;
                break;
            }
            value.push(c);
        }
        out.push((key.trim().to_string(), value));
        rest = body[end..]
            .trim_start()
            .trim_start_matches(',')
            .trim_start();
    }
    out
}

/// The templates (`pg_ts_template`).
pub const SIMPLE: i64 = 3727;
pub const SYNONYM: i64 = 3730;
pub const ISPELL: i64 = 3733;
pub const THESAURUS: i64 = 3742;
pub const SNOWBALL: i64 = 13622;

/// A template's oid by (possibly `pg_catalog`-qualified) name.
pub fn template_oid(name: &[String]) -> Option<i64> {
    let bare = match name {
        [n] => n.as_str(),
        [s, n] if s == "pg_catalog" => n.as_str(),
        _ => return None,
    };
    Some(match bare {
        "simple" => SIMPLE,
        "synonym" => SYNONYM,
        "ispell" => ISPELL,
        "thesaurus" => THESAURUS,
        "snowball" => SNOWBALL,
        _ => return None,
    })
}

/// A template's name by oid.
pub fn template_name(oid: i64) -> Option<&'static str> {
    Some(match oid {
        SIMPLE => "simple",
        SYNONYM => "synonym",
        ISPELL => "ispell",
        THESAURUS => "thesaurus",
        SNOWBALL => "snowball",
        _ => return None,
    })
}

/// What a dictionary does with a token, its options read.
#[derive(Debug, Clone)]
pub enum Lexer {
    Simple {
        stop: Option<Vec<String>>,
        accept: bool,
    },
    Snowball {
        language: String,
        stop: Option<Vec<String>>,
    },
    Synonym {
        /// `(in, out, prefix)`, sorted by `in`.
        entries: Vec<(String, String, bool)>,
        case_sensitive: bool,
    },
}

/// The share directory a missing dictionary file's message names: a
/// PostgreSQL built from source's default (the installation's own path is
/// what a real server prints).
const SHAREDIR: &str = "/usr/local/pgsql/share";

/// The data directory PostgreSQL reads a dictionary file from, as its
/// messages print it.
fn tsearch_data_path(file: &str) -> String {
    format!("{SHAREDIR}/tsearch_data/{file}")
}

/// `get_tsearch_config_filename`'s check: a name of lower-case letters,
/// digits and underscores.
fn config_file_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(Error::Sqlstate(
            "22023",
            format!("invalid text search configuration file name \"{name}\""),
        ));
    }
    Ok(())
}

/// `readstoplist`: a stop-word file's words, lower-cased.
fn stop_list(name: &str) -> Result<Vec<String>> {
    config_file_name(name)?;
    let words = crate::fts::stop_words(name).ok_or_else(|| {
        Error::Sqlstate(
            "F0000",
            format!(
                "could not open stop-word file \"{}\": No such file or directory",
                tsearch_data_path(&format!("{name}.stop"))
            ),
        )
    })?;
    Ok(words)
}

/// `defGetBoolean`.
fn boolean(name: &str, v: Option<&str>) -> Result<bool> {
    let Some(v) = v else {
        return Ok(true);
    };
    match v.to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "t" | "yes" | "y" => Ok(true),
        "false" | "off" | "0" | "f" | "no" | "n" => Ok(false),
        _ => Err(Error::Sqlstate(
            "42601",
            format!("{name} requires a Boolean value"),
        )),
    }
}

/// The sample synonym file PostgreSQL ships (`synonym_sample.syn`).
const SYNONYM_SAMPLE: &str = include_str!("tsearch_data/synonym_sample.syn");

/// A template's `init`: its options checked and read.
pub fn init(template: i64, options: &[(String, String)]) -> Result<Lexer> {
    let value = |k: &str| {
        options
            .iter()
            .find(|(o, _)| o == k)
            .map(|(_, v)| v.as_str())
    };
    let once = |k: &str, what: &str| -> Result<()> {
        if options.iter().filter(|(o, _)| o == k).count() > 1 {
            return Err(Error::Sqlstate(
                "22023",
                format!("multiple {what} parameters"),
            ));
        }
        Ok(())
    };
    match template {
        SIMPLE => {
            if let Some((k, _)) = options
                .iter()
                .find(|(k, _)| !matches!(k.as_str(), "stopwords" | "accept"))
            {
                return Err(Error::Sqlstate(
                    "22023",
                    format!("unrecognized simple dictionary parameter: \"{k}\""),
                ));
            }
            once("stopwords", "StopWords")?;
            once("accept", "Accept")?;
            let stop = value("stopwords").map(stop_list).transpose()?;
            let accept = match options.iter().find(|(k, _)| k == "accept") {
                Some((_, v)) => boolean("accept", Some(v))?,
                None => true,
            };
            Ok(Lexer::Simple { stop, accept })
        }
        SNOWBALL => {
            let mut stop = None;
            let mut language: Option<String> = None;
            for (k, v) in options {
                match k.as_str() {
                    "stopwords" => {
                        if stop.is_some() {
                            return Err(Error::Sqlstate(
                                "22023",
                                "multiple StopWords parameters".into(),
                            ));
                        }
                        stop = Some(stop_list(v)?);
                    }
                    "language" => {
                        if language.is_some() {
                            return Err(Error::Sqlstate(
                                "22023",
                                "multiple Language parameters".into(),
                            ));
                        }
                        let lang = v.to_ascii_lowercase();
                        if !crate::fts::stemmer_exists(&lang) {
                            return Err(Error::Sqlstate(
                                "42704",
                                format!(
                                    "no Snowball stemmer available for language \"{v}\" and encoding \"UTF8\""
                                ),
                            ));
                        }
                        language = Some(lang);
                    }
                    other => {
                        return Err(Error::Sqlstate(
                            "22023",
                            format!("unrecognized Snowball parameter: \"{other}\""),
                        ))
                    }
                }
            }
            let language = language
                .ok_or_else(|| Error::Sqlstate("22023", "missing Language parameter".into()))?;
            Ok(Lexer::Snowball { language, stop })
        }
        SYNONYM => {
            let mut file = None;
            let mut case_sensitive = false;
            for (k, v) in options {
                match k.as_str() {
                    "synonyms" => file = Some(v.clone()),
                    "casesensitive" => case_sensitive = boolean("casesensitive", Some(v))?,
                    other => {
                        return Err(Error::Sqlstate(
                            "22023",
                            format!("unrecognized synonym parameter: \"{other}\""),
                        ))
                    }
                }
            }
            let file =
                file.ok_or_else(|| Error::Sqlstate("22023", "missing Synonyms parameter".into()))?;
            config_file_name(&file)?;
            let text = match file.as_str() {
                "synonym_sample" => SYNONYM_SAMPLE,
                _ => {
                    return Err(Error::Sqlstate(
                        "F0000",
                        format!(
                            "could not open synonym file \"{}\": No such file or directory",
                            tsearch_data_path(&format!("{file}.syn"))
                        ),
                    ))
                }
            };
            let lower = |s: &str| -> String {
                if case_sensitive {
                    s.to_string()
                } else {
                    s.chars().map(crate::scalar::simple_lower).collect()
                }
            };
            let mut entries: Vec<(String, String, bool)> = text
                .lines()
                .filter_map(|l| {
                    let mut words = l.split_whitespace();
                    let input = words.next()?;
                    let output = words.next()?;
                    let (output, prefix) = match output.strip_suffix('*') {
                        Some(o) if !o.is_empty() => (o, true),
                        _ => (output, false),
                    };
                    Some((lower(input), lower(output), prefix))
                })
                .collect();
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            Ok(Lexer::Synonym {
                entries,
                case_sensitive,
            })
        }
        ISPELL | THESAURUS => Err(Error::FeatureNotSupported(format!(
            "text search dictionaries of the {} template are not supported",
            template_name(template).unwrap_or_default()
        ))),
        _ => Err(Error::Sqlstate(
            "42704",
            format!("text search template {template} does not exist"),
        )),
    }
}

/// A template's `lexize`: the lexemes (an empty list for a stop word), or
/// `None` when the dictionary does not recognise the token.
pub fn lexize(lexer: &Lexer, token: &str) -> Option<Vec<String>> {
    let lower = || -> String { token.chars().map(crate::scalar::simple_lower).collect() };
    match lexer {
        Lexer::Simple { stop, accept } => {
            let txt = lower();
            if txt.is_empty() || stop.as_ref().is_some_and(|s| s.contains(&txt)) {
                Some(Vec::new())
            } else if *accept {
                Some(vec![txt])
            } else {
                None
            }
        }
        Lexer::Snowball { language, stop } => {
            let txt = lower();
            if token.len() > 1000 {
                return Some(vec![txt]);
            }
            if txt.is_empty() || stop.as_ref().is_some_and(|s| s.contains(&txt)) {
                return Some(Vec::new());
            }
            Some(vec![crate::fts::stem_in(language, &txt)])
        }
        Lexer::Synonym {
            entries,
            case_sensitive,
        } => {
            if token.is_empty() || entries.is_empty() {
                return None;
            }
            let key = if *case_sensitive {
                token.to_string()
            } else {
                lower()
            };
            entries
                .binary_search_by(|e| e.0.as_bytes().cmp(key.as_bytes()))
                .ok()
                .map(|i| vec![entries[i].1.clone()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip() {
        let opts = vec![
            ("stopwords".to_string(), "english".to_string()),
            ("accept".to_string(), "it's".to_string()),
        ];
        let text = options_text(&opts).unwrap();
        assert_eq!(text, "stopwords = 'english', accept = 'it''s'");
        assert_eq!(parse_options_text(&text), opts);
    }

    #[test]
    fn synonym_sample() {
        let l = init(SYNONYM, &[("synonyms".into(), "synonym_sample".into())]).unwrap();
        assert_eq!(lexize(&l, "Postgres"), Some(vec!["pgsql".to_string()]));
        assert_eq!(lexize(&l, "indices"), Some(vec!["index".to_string()]));
        assert_eq!(lexize(&l, "nothing"), None);
    }
}
