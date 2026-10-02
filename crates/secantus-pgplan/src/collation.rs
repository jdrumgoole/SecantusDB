//! Collations: `COLLATE "und-x-icu"`, a column's declared collation, and
//! `CREATE COLLATION` (the executor stores those and installs them here).
//!
//! This database's own collation is `C` (as the reference's `datcollate`),
//! so text compares by bytes unless a collation says otherwise. `C`, `POSIX`,
//! `ucs_basic` and `default` are that byte order. Any other collation is an
//! ICU one -- a `<language tag>-x-icu` name, or a `CREATE COLLATION` with
//! `provider = icu` -- evaluated with ICU4X over the same CLDR data ICU4C
//! uses. Its locale's `-u-` keywords are honoured: `ks` (strength), `kn`
//! (numeric ordering), `kf` (case first), `co` (the collation type) and `ka`
//! (alternate handling).
//!
//! Order-sensitive operations over a collated value read its SORT KEY
//! (`__coll_key(collation, text)`, see `enum_order`), whose string order is
//! the collation's. A DETERMINISTIC collation breaks a tie by the text's
//! bytes, as PostgreSQL's does, so only identical strings compare equal; a
//! nondeterministic one (`deterministic = false`) compares equal whatever
//! its strength calls equal (`und-u-ks-level2` is case-insensitive).
//!
//! The key carries the original text after a `|`, so `min` / `max` can
//! hand the value back (`__coll_value`).

use super::*;
use icu_collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};

/// One collation `CREATE COLLATION` made.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserCollation {
    pub name: String,
    /// `i` (icu), `c` (libc) or `d` (default).
    pub provider: char,
    /// The ICU locale (`und-u-ks-level2`), or the libc `lc_collate`.
    pub locale: String,
    pub deterministic: bool,
    pub oid: i64,
}

thread_local! {
    static USER_COLLATIONS: std::cell::RefCell<Vec<UserCollation>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static COLLATORS: std::cell::RefCell<std::collections::HashMap<String, CollatorBorrowed<'static>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Install the database's collations for the statements that follow.
pub fn set_user_collations(collations: Vec<UserCollation>) {
    USER_COLLATIONS.with(|c| *c.borrow_mut() = collations);
}

/// The installed user collations.
pub fn user_collations() -> Vec<UserCollation> {
    USER_COLLATIONS.with(|c| c.borrow().clone())
}

/// What a collation name resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    /// Byte order: `C` and its equivalents.
    Bytes,
    Icu {
        locale: String,
        deterministic: bool,
    },
    /// citext's comparison: `lower(a)` against `lower(b)` in byte order,
    /// nondeterministic (`Apple` and `apple` are equal). Reached only by
    /// [`CITEXT`], the collation a citext value carries implicitly.
    Lower,
}

/// The internal collation name a `citext` value carries implicitly: its
/// comparisons, ordering, grouping and uniqueness all go through it.
pub const CITEXT: &str = "__citext";

/// The collation a column compares under: its declared one, else citext's
/// implicit one for a `citext` (or `citext[]`) column.
pub fn column_collation(column: &Column) -> Option<String> {
    column
        .extra
        .get_str("collation")
        .ok()
        .map(str::to_string)
        .or_else(|| is_citext(&column.pg_type).then(|| CITEXT.to_string()))
}

/// Is `ty` citext (with the extension installed)?
pub fn is_citext(ty: &str) -> bool {
    let t = ty.trim();
    let t = t.strip_prefix("public.").unwrap_or(t);
    t.eq_ignore_ascii_case("citext") && crate::extension_installed("citext")
}

impl Resolved {
    /// Does ordering under it differ from byte order?
    pub fn is_locale(&self) -> bool {
        matches!(self, Resolved::Icu { .. } | Resolved::Lower)
    }

    pub fn deterministic(&self) -> bool {
        !matches!(
            self,
            Resolved::Icu {
                deterministic: false,
                ..
            } | Resolved::Lower
        )
    }
}

fn does_not_exist(name: &str) -> Error {
    Error::Sqlstate(
        "42704",
        format!("collation \"{name}\" for encoding \"UTF8\" does not exist"),
    )
}

/// A libc locale (`en_US.UTF-8`) as the ICU locale it names (`en-US`).
fn libc_to_icu(locale: &str) -> String {
    let base = locale.split(['.', '@']).next().unwrap_or(locale);
    base.replace('_', "-")
}

/// Resolve a collation by name: a built-in, a user collation, or an
/// `<tag>-x-icu` name. `42704` when there is none.
pub fn resolve(name: &str) -> Result<Resolved> {
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if matches!(name, "C" | "POSIX" | "ucs_basic" | "default") {
        return Ok(Resolved::Bytes);
    }
    if name == CITEXT {
        return Ok(Resolved::Lower);
    }
    if let Some(u) = USER_COLLATIONS.with(|c| c.borrow().iter().find(|u| u.name == name).cloned()) {
        return Ok(match u.provider {
            'c' | 'd' if matches!(u.locale.as_str(), "C" | "POSIX" | "") => Resolved::Bytes,
            'c' | 'd' => Resolved::Icu {
                locale: libc_to_icu(&u.locale),
                deterministic: u.deterministic,
            },
            _ => Resolved::Icu {
                locale: u.locale.clone(),
                deterministic: u.deterministic,
            },
        });
    }
    if let Some(tag) = name.strip_suffix("-x-icu") {
        if tag.parse::<icu_locale::Locale>().is_ok() {
            return Ok(Resolved::Icu {
                locale: tag.to_string(),
                deterministic: true,
            });
        }
    }
    Err(does_not_exist(name))
}

/// Is `locale` one ICU can build a collator for? (`CREATE COLLATION`'s
/// check.)
pub fn valid_icu_locale(locale: &str) -> bool {
    build(locale).is_some()
}

/// The value of a `-u-` keyword in a locale tag (`ks` of `und-u-ks-level2`).
fn keyword<'a>(locale: &'a str, key: &str) -> Option<&'a str> {
    let lower = locale;
    let parts: Vec<&str> = lower.split(['-', '_']).collect();
    let u = parts.iter().position(|p| p.eq_ignore_ascii_case("u"))?;
    let rest = &parts[u + 1..];
    let i = rest.iter().position(|p| p.eq_ignore_ascii_case(key))?;
    rest.get(i + 1).copied().filter(|v| v.len() > 2)
}

fn build(locale: &str) -> Option<CollatorBorrowed<'static>> {
    let parsed: icu_locale::Locale = locale.parse().ok()?;
    let prefs = CollatorPreferences::from(&parsed);
    let mut options = CollatorOptions::default();
    options.strength = match keyword(locale, "ks")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("level1") => Some(Strength::Primary),
        Some("level2") => Some(Strength::Secondary),
        Some("level3") => Some(Strength::Tertiary),
        Some("level4") => Some(Strength::Quaternary),
        Some("identic") => Some(Strength::Identical),
        _ => None,
    };
    if keyword(locale, "ka").is_some_and(|v| v.eq_ignore_ascii_case("shifted")) {
        options.alternate_handling = Some(AlternateHandling::Shifted);
    }
    Collator::try_new(prefs, options).ok()
}

/// The sort key of `text` under ICU `locale`, as bytes.
fn icu_key(locale: &str, text: &str) -> Result<Vec<u8>> {
    COLLATORS.with(|c| {
        let mut cache = c.borrow_mut();
        if !cache.contains_key(locale) {
            let collator = build(locale).ok_or_else(|| {
                Error::Sqlstate("22023", format!("could not create locale \"{locale}\""))
            })?;
            cache.insert(locale.to_string(), collator);
        }
        let mut key = Vec::new();
        let _ = cache[locale].write_sort_key_to(text, &mut key);
        Ok(key)
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `__coll_key(collation, text)`: a string whose order AND equality are the
/// collation's. See the module header.
pub fn sort_key(collation: &str, text: &str) -> Result<String> {
    Ok(match resolve(collation)? {
        // Byte order: the text's own bytes, hex, keep that order.
        Resolved::Bytes => hex(text.as_bytes()),
        Resolved::Lower => hex(lower(text).as_bytes()),
        Resolved::Icu {
            locale,
            deterministic,
        } => {
            let key = hex(&icu_key(&locale, text)?);
            if deterministic {
                format!("{key}!{}", hex(text.as_bytes()))
            } else {
                key
            }
        }
    })
}

/// citext's `lower`: the database's (simple, per-character) case mapping.
pub fn lower(text: &str) -> String {
    text.chars().map(crate::scalar::simple_lower).collect()
}

/// `__coll_keyv(collation, text)`: the sort key carrying its text after a
/// `|`, so an extreme (`min`, `greatest`) can be read back.
pub fn sort_key_with_value(collation: &str, text: &str) -> Result<String> {
    Ok(format!("{}|{text}", sort_key(collation, text)?))
}

/// `__coll_value(key)`: the text a key was made from.
pub fn key_value(key: &str) -> String {
    key.split_once('|')
        .map_or_else(String::new, |(_, t)| t.to_string())
}

/// The collatable types.
pub fn collatable(ty: &str) -> bool {
    matches!(
        ty,
        "text"
            | "varchar"
            | "bpchar"
            | "name"
            | "unknown"
            | "citext"
            | "character varying"
            | "character"
    ) || ty.ends_with("[]") && collatable(ty.trim_end_matches("[]"))
}

/// A `COLLATE` clause's collation name.
pub fn clause_name(cc: &pg_query::protobuf::CollateClause) -> String {
    cc.collname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

fn def_text(arg: Option<&pg_query::protobuf::Node>) -> Option<String> {
    match arg?.node.as_ref()? {
        N::String(s) => Some(s.sval.clone()),
        N::TypeName(t) => Some(type_name_of(t)),
        N::Boolean(b) => Some(b.boolval.to_string()),
        N::Integer(i) => Some(i.ival.to_string()),
        N::List(l) => l.items.iter().rev().find_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn truth(v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" | "t" => Ok(true),
        "false" | "off" | "no" | "0" | "f" => Ok(false),
        _ => Err(Error::Sqlstate(
            "22023",
            "deterministic requires a Boolean value".into(),
        )),
    }
}

/// `CREATE COLLATION [IF NOT EXISTS] name (provider = ..., locale = ...,
/// deterministic = ...)` or `... FROM existing`.
pub(crate) fn plan_create(d: &pg_query::protobuf::DefineStmt) -> Result<Statement> {
    let name = d
        .defnames
        .iter()
        .rev()
        .find_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let (mut provider, mut locale, mut lc_collate, mut lc_ctype, mut deterministic, mut from) = (
        None::<String>,
        None::<String>,
        None::<String>,
        None::<String>,
        None::<bool>,
        None::<String>,
    );
    for e in &d.definition {
        let Some(N::DefElem(e)) = e.node.as_ref() else {
            continue;
        };
        let v = def_text(e.arg.as_deref());
        match e.defname.to_ascii_lowercase().as_str() {
            "provider" => provider = v,
            "locale" => locale = v,
            "lc_collate" => lc_collate = v,
            "lc_ctype" => lc_ctype = v,
            "deterministic" => deterministic = Some(truth(&v.unwrap_or_default())?),
            "version" => {}
            "from" => from = v,
            other => {
                set_error_location(e.location);
                return Err(Error::Sqlstate(
                    "42601",
                    format!("collation attribute \"{other}\" not recognized"),
                ));
            }
        }
    }
    let collation = if let Some(source) = from {
        match source.as_str() {
            "C" | "POSIX" | "ucs_basic" | "default" => UserCollation {
                name,
                provider: 'c',
                locale: if source == "POSIX" {
                    "POSIX".into()
                } else {
                    "C".into()
                },
                deterministic: true,
                oid: 0,
            },
            _ => match user_collations().into_iter().find(|u| u.name == source) {
                Some(u) => UserCollation { name, oid: 0, ..u },
                None => match source.strip_suffix("-x-icu") {
                    Some(tag) if tag.parse::<icu_locale::Locale>().is_ok() => UserCollation {
                        name,
                        provider: 'i',
                        locale: tag.to_string(),
                        deterministic: true,
                        oid: 0,
                    },
                    _ => return Err(does_not_exist(&source)),
                },
            },
        }
    } else {
        let provider = match provider.as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("libc") => 'c',
            Some("icu") => 'i',
            Some(other) => {
                return Err(Error::Sqlstate(
                    "42P17",
                    format!("unrecognized collation provider: {other}"),
                ))
            }
        };
        let deterministic = deterministic.unwrap_or(true);
        if provider == 'c' && !deterministic {
            return Err(Error::FeatureNotSupported(
                "nondeterministic collations not supported with this provider".into(),
            ));
        }
        let locale = if provider == 'i' {
            locale.ok_or_else(|| {
                Error::Sqlstate("42P17", "parameter \"locale\" must be specified".into())
            })?
        } else {
            match (locale, lc_collate, lc_ctype) {
                (Some(l), _, _) => l,
                (None, Some(c), Some(_)) => c,
                (None, None, _) => {
                    return Err(Error::Sqlstate(
                        "42P17",
                        "parameter \"lc_collate\" must be specified".into(),
                    ))
                }
                (None, Some(_), None) => {
                    return Err(Error::Sqlstate(
                        "42P17",
                        "parameter \"lc_ctype\" must be specified".into(),
                    ))
                }
            }
        };
        UserCollation {
            name,
            provider,
            locale,
            deterministic,
            oid: 0,
        }
    };
    Ok(Statement::CreateCollation {
        collation,
        if_not_exists: d.if_not_exists,
    })
}

/// `DROP COLLATION [IF EXISTS] name [, ...] [CASCADE]`.
pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    let names = d
        .objects
        .iter()
        .filter_map(|o| match o.node.as_ref() {
            Some(N::List(l)) => l.items.iter().rev().find_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            }),
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect();
    Ok(Statement::DropCollation {
        names,
        if_exists: d.missing_ok,
        cascade: pg_query::protobuf::DropBehavior::try_from(d.behavior)
            == Ok(pg_query::protobuf::DropBehavior::DropCascade),
    })
}
