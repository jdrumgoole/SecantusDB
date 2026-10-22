//! COPY's options, read and checked as PostgreSQL 15's `ProcessCopyOptions`
//! reads and checks them (`commands/copy.c`), in its order and with its
//! SQLSTATEs (RP-073). The formats themselves -- writing and reading a
//! row under these options -- are the wire layer's.

use super::*;

/// `FORCE_QUOTE`'s argument.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ForceQuote {
    #[default]
    None,
    /// `FORCE_QUOTE *`.
    All,
    Columns(Vec<String>),
}

/// Every COPY option but FORMAT and HEADER, which `CopyFrom` carries itself.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyOptions {
    /// One byte: tab for text, comma for CSV by default.
    pub delimiter: char,
    /// The NULL string: `\N` for text, the empty string for CSV.
    pub null: String,
    /// CSV only: the quote and escape characters (`"` and the quote).
    pub quote: char,
    pub escape: char,
    pub force_quote: ForceQuote,
    pub force_not_null: Vec<String>,
    pub force_null: Vec<String>,
    pub freeze: bool,
    /// `ENCODING`: the data's encoding, instead of the client encoding.
    pub encoding: Option<String>,
}

impl Default for CopyOptions {
    fn default() -> Self {
        CopyOptions {
            delimiter: '\t',
            null: "\\N".into(),
            quote: '"',
            escape: '"',
            force_quote: ForceQuote::None,
            force_not_null: Vec::new(),
            force_null: Vec::new(),
            freeze: false,
            encoding: None,
        }
    }
}

fn redundant() -> Error {
    Error::Sqlstate("42601", "conflicting or redundant options".into())
}

fn unsupported(m: &str) -> Error {
    Error::Sqlstate("0A000", m.into())
}

fn invalid(m: String) -> Error {
    Error::Sqlstate("22023", m)
}

/// An option's value as text (`defGetString`), `None` when it has none.
fn string_arg(d: &pg_query::protobuf::DefElem) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    match d.arg.as_deref()?.node.as_ref()? {
        N::String(s) => Some(s.sval.clone()),
        N::Integer(i) => Some(i.ival.to_string()),
        N::Float(f) => Some(f.fval.clone()),
        N::Boolean(b) => Some(if b.boolval { "true" } else { "false" }.into()),
        N::AConst(c) => match c.val.as_ref()? {
            Val::Sval(s) => Some(s.sval.clone()),
            Val::Ival(i) => Some(i.ival.to_string()),
            Val::Fval(f) => Some(f.fval.clone()),
            Val::Boolval(b) => Some(if b.boolval { "true" } else { "false" }.into()),
            Val::Bsval(b) => Some(b.bsval.clone()),
        },
        _ => None,
    }
}

/// `defGetBoolean`: no value is true.
fn bool_arg(d: &pg_query::protobuf::DefElem) -> Result<bool> {
    let Some(v) = string_arg(d) else {
        return Ok(true);
    };
    match v.to_ascii_lowercase().as_str() {
        "true" | "on" | "1" => Ok(true),
        "false" | "off" | "0" => Ok(false),
        _ => Err(Error::Sqlstate(
            "42601",
            format!("{} requires a Boolean value", d.defname),
        )),
    }
}

/// A `FORCE_*` column list; `Ok(None)` for `*`.
fn column_list(d: &pg_query::protobuf::DefElem, star_ok: bool) -> Result<Option<Vec<String>>> {
    let not_list = || {
        Error::Sqlstate(
            "22023",
            format!(
                "argument to option \"{}\" must be a list of column names",
                d.defname
            ),
        )
    };
    match d.arg.as_deref().and_then(|a| a.node.as_ref()) {
        Some(N::AStar(_)) if star_ok => Ok(None),
        Some(N::List(l)) => l
            .items
            .iter()
            .map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Ok(s.sval.clone()),
                _ => Err(not_list()),
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        _ => Err(not_list()),
    }
}

/// What `plan_copy` reads off the statement: the format, HEADER, and the
/// rest as `CopyOptions`.
pub(crate) struct Parsed {
    pub format: CopyFormat,
    pub header: bool,
    pub options: CopyOptions,
}

/// `ProcessCopyOptions`. `columns` is the COPY's column list resolved (every
/// column when the statement names none), for the `FORCE_*` lists.
pub(crate) fn process(
    options: &[pg_query::protobuf::Node],
    is_from: bool,
    columns: &[String],
    relation: Option<(&str, &[String])>,
) -> Result<Parsed> {
    let mut format: Option<CopyFormat> = None;
    let mut header: Option<bool> = None;
    let mut delimiter: Option<String> = None;
    let mut null: Option<String> = None;
    let mut quote: Option<String> = None;
    let mut escape: Option<String> = None;
    let mut force_quote: Option<ForceQuote> = None;
    let mut force_not_null: Option<Vec<String>> = None;
    let mut force_null: Option<Vec<String>> = None;
    let mut freeze: Option<bool> = None;
    let mut encoding: Option<String> = None;
    for opt in options {
        let Some(N::DefElem(d)) = opt.node.as_ref() else {
            continue;
        };
        let set_str = |slot: &mut Option<String>| -> Result<()> {
            if slot.is_some() {
                return Err(redundant());
            }
            *slot = Some(string_arg(d).unwrap_or_default());
            Ok(())
        };
        match d.defname.as_str() {
            "format" => {
                if format.is_some() {
                    return Err(redundant());
                }
                let v = string_arg(d).unwrap_or_default();
                format = Some(match v.to_ascii_lowercase().as_str() {
                    "text" => CopyFormat::Text,
                    "csv" => CopyFormat::Csv,
                    "binary" => CopyFormat::Binary,
                    _ => {
                        return Err(invalid(format!("COPY format \"{v}\" not recognized")));
                    }
                });
            }
            "freeze" => {
                if freeze.is_some() {
                    return Err(redundant());
                }
                freeze = Some(bool_arg(d)?);
            }
            "delimiter" => set_str(&mut delimiter)?,
            "null" => set_str(&mut null)?,
            "header" => {
                if header.is_some() {
                    return Err(redundant());
                }
                // `HEADER MATCH` (COPY FROM) checks the names, which is
                // refused rather than skipped unchecked.
                match string_arg(d).map(|v| v.to_ascii_lowercase()).as_deref() {
                    Some("match") => {
                        return Err(Error::Unsupported("COPY ... HEADER MATCH".into()))
                    }
                    None | Some("true" | "on" | "1") => header = Some(true),
                    Some("false" | "off" | "0") => header = Some(false),
                    Some(_) => {
                        return Err(Error::Sqlstate(
                            "42601",
                            format!("{} requires a Boolean value or \"match\"", d.defname),
                        ))
                    }
                }
            }
            "quote" => set_str(&mut quote)?,
            "escape" => set_str(&mut escape)?,
            "force_quote" => {
                if force_quote.is_some() {
                    return Err(redundant());
                }
                force_quote = Some(match column_list(d, true)? {
                    None => ForceQuote::All,
                    Some(cols) => ForceQuote::Columns(cols),
                });
            }
            "force_not_null" => {
                if force_not_null.is_some() {
                    return Err(redundant());
                }
                force_not_null = column_list(d, false)?;
            }
            "force_null" => {
                if force_null.is_some() {
                    return Err(redundant());
                }
                force_null = column_list(d, false)?;
            }
            "encoding" => {
                if encoding.is_some() {
                    return Err(redundant());
                }
                let v = string_arg(d).unwrap_or_default();
                let Some(canonical) = crate::builtins_text::encoding_by_name(&v) else {
                    return Err(invalid(format!(
                        "argument to option \"{}\" must be a valid encoding name",
                        d.defname
                    )));
                };
                encoding = Some(canonical.to_string());
            }
            other => {
                return Err(Error::Sqlstate(
                    "42601",
                    format!("option \"{other}\" not recognized"),
                ))
            }
        }
    }
    let format = format.unwrap_or_default();
    let csv = format == CopyFormat::Csv;
    let binary = format == CopyFormat::Binary;
    // Binary mode takes no DELIMITER and no NULL.
    if binary && delimiter.is_some() {
        return Err(Error::Sqlstate(
            "42601",
            "cannot specify DELIMITER in BINARY mode".into(),
        ));
    }
    if binary && null.is_some() {
        return Err(Error::Sqlstate(
            "42601",
            "cannot specify NULL in BINARY mode".into(),
        ));
    }
    let delimiter = delimiter.unwrap_or_else(|| (if csv { "," } else { "\t" }).into());
    let null = null.unwrap_or_else(|| (if csv { "" } else { "\\N" }).into());
    // Only single-byte delimiter strings are supported.
    if delimiter.len() != 1 {
        return Err(unsupported(
            "COPY delimiter must be a single one-byte character",
        ));
    }
    if delimiter.contains(['\r', '\n']) {
        return Err(invalid(
            "COPY delimiter cannot be newline or carriage return".into(),
        ));
    }
    if null.contains(['\r', '\n']) {
        return Err(invalid(
            "COPY null representation cannot use newline or carriage return".into(),
        ));
    }
    // An unsafe delimiter outside CSV.
    if !csv && "\\.abcdefghijklmnopqrstuvwxyz0123456789".contains(delimiter.as_str()) {
        return Err(invalid(format!("COPY delimiter cannot be \"{delimiter}\"")));
    }
    let header = header.unwrap_or(false);
    if binary && header {
        return Err(unsupported("cannot specify HEADER in BINARY mode"));
    }
    if !csv && quote.is_some() {
        return Err(unsupported("COPY quote available only in CSV mode"));
    }
    let quote = quote.unwrap_or_else(|| "\"".into());
    if csv && quote.len() != 1 {
        return Err(unsupported(
            "COPY quote must be a single one-byte character",
        ));
    }
    if csv && delimiter == quote {
        return Err(invalid("COPY delimiter and quote must be different".into()));
    }
    if !csv && escape.is_some() {
        return Err(unsupported("COPY escape available only in CSV mode"));
    }
    let escape = escape.unwrap_or_else(|| quote.clone());
    if csv && escape.len() != 1 {
        return Err(unsupported(
            "COPY escape must be a single one-byte character",
        ));
    }
    if !csv && force_quote.is_some() {
        return Err(unsupported("COPY force quote available only in CSV mode"));
    }
    if force_quote.is_some() && is_from {
        return Err(unsupported("COPY force quote only available using COPY TO"));
    }
    if !csv && force_not_null.is_some() {
        return Err(unsupported(
            "COPY force not null available only in CSV mode",
        ));
    }
    if force_not_null.is_some() && !is_from {
        return Err(unsupported(
            "COPY force not null only available using COPY FROM",
        ));
    }
    if !csv && force_null.is_some() {
        return Err(unsupported("COPY force null available only in CSV mode"));
    }
    if force_null.is_some() && !is_from {
        return Err(unsupported(
            "COPY force null only available using COPY FROM",
        ));
    }
    if null.contains(delimiter.as_str()) {
        return Err(unsupported(
            "COPY delimiter must not appear in the NULL specification",
        ));
    }
    if csv && null.contains(quote.as_str()) {
        return Err(unsupported(
            "CSV quote character must not appear in the NULL specification",
        ));
    }
    // Each FORCE_* column must be one the COPY reads or writes (BeginCopy).
    let referenced = |list: &[String], option: &str| -> Result<()> {
        for name in list {
            // A name the relation does not have is `CopyGetAttnums`' 42703.
            let known = match relation {
                Some((_, all)) => all.iter().any(|c| c == name),
                None => columns.iter().any(|c| c == name),
            };
            if !known {
                return Err(Error::Sqlstate(
                    "42703",
                    match relation {
                        Some((rel, _)) => {
                            format!("column \"{name}\" of relation \"{rel}\" does not exist")
                        }
                        None => format!("column \"{name}\" does not exist"),
                    },
                ));
            }
            if !columns.iter().any(|c| c == name) {
                return Err(Error::Sqlstate(
                    "42P10",
                    format!("{option} column \"{name}\" not referenced by COPY"),
                ));
            }
        }
        Ok(())
    };
    if let Some(ForceQuote::Columns(cols)) = &force_quote {
        referenced(cols, "FORCE_QUOTE")?;
    }
    if let Some(cols) = &force_not_null {
        referenced(cols, "FORCE_NOT_NULL")?;
    }
    if let Some(cols) = &force_null {
        referenced(cols, "FORCE_NULL")?;
    }
    let one = |s: &str| s.chars().next().expect("checked one byte");
    Ok(Parsed {
        format,
        header,
        options: CopyOptions {
            delimiter: one(&delimiter),
            null,
            quote: one(&quote),
            escape: one(&escape),
            force_quote: force_quote.unwrap_or_default(),
            force_not_null: force_not_null.unwrap_or_default(),
            force_null: force_null.unwrap_or_default(),
            freeze: freeze.unwrap_or(false),
            encoding,
        },
    })
}
