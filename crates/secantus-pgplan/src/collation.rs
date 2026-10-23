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
    static COLLATORS: std::cell::RefCell<std::collections::HashMap<String, Built>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

thread_local! {
    /// Does any column (or domain) of the database declare a collation?
    /// `true` until the server says otherwise: see [`text_is_byte_ordered`].
    static DECLARED_COLLATIONS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Record whether any column or domain of the database declares a
/// collation (the server installs it with the catalog).
pub fn set_declared_collations(any: bool) {
    DECLARED_COLLATIONS.with(|c| c.set(any));
}

/// Is a plain `text` / `varchar` / `name` value's order byte order -- no
/// column or domain declaring a collation, no user collation? Then two such
/// values compare as their bytes, whatever expression produced them.
pub fn text_is_byte_ordered() -> bool {
    !DECLARED_COLLATIONS.with(std::cell::Cell::get)
        && USER_COLLATIONS.with(|c| c.borrow().is_empty())
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

/// Is `ty` citext, with the extension's operators on the search path? A
/// citext whose schema the path does not reach compares as text, as on
/// PostgreSQL, where its `=` is then not found.
pub fn is_citext(ty: &str) -> bool {
    is_citext_type(ty) && crate::extension_visible("citext")
}

/// The functions the citext extension adds that no other type has, by
/// name, as `(name, arguments, the SQL they compute)` -- `$n` an argument.
/// Each is the extension's C function's rule (citext.c, 1.6): compare the
/// lowered strings, by the database's collation or (`pattern`) bytewise.
pub const CITEXT_ONLY_FUNCTIONS: &[(&str, usize, &str)] = &[
    ("citext", 1, "(($1)::text)::citext"),
    ("citext_eq", 2, "(lower(($1)::text) = lower(($2)::text))"),
    ("citext_ne", 2, "(lower(($1)::text) <> lower(($2)::text))"),
    ("citext_lt", 2, "(lower(($1)::text) < lower(($2)::text))"),
    ("citext_le", 2, "(lower(($1)::text) <= lower(($2)::text))"),
    ("citext_gt", 2, "(lower(($1)::text) > lower(($2)::text))"),
    ("citext_ge", 2, "(lower(($1)::text) >= lower(($2)::text))"),
    (
        "citext_cmp",
        2,
        "(case when lower(($1)::text) < lower(($2)::text) then -1 \
         when lower(($1)::text) = lower(($2)::text) then 0 else 1 end)",
    ),
    (
        "citext_larger",
        2,
        "((case when lower(($1)::text) > lower(($2)::text) then ($1)::text \
         else ($2)::text end)::citext)",
    ),
    (
        "citext_smaller",
        2,
        "((case when lower(($1)::text) < lower(($2)::text) then ($1)::text \
         else ($2)::text end)::citext)",
    ),
    ("citext_hash", 1, "hashtext(lower(($1)::text))"),
    (
        "citext_hash_extended",
        2,
        "hashtextextended(lower(($1)::text), $2)",
    ),
    (
        "citext_pattern_lt",
        2,
        "(lower(($1)::text) collate \"C\" < lower(($2)::text) collate \"C\")",
    ),
    (
        "citext_pattern_le",
        2,
        "(lower(($1)::text) collate \"C\" <= lower(($2)::text) collate \"C\")",
    ),
    (
        "citext_pattern_gt",
        2,
        "(lower(($1)::text) collate \"C\" > lower(($2)::text) collate \"C\")",
    ),
    (
        "citext_pattern_ge",
        2,
        "(lower(($1)::text) collate \"C\" >= lower(($2)::text) collate \"C\")",
    ),
    (
        "citext_pattern_cmp",
        2,
        "(case when lower(($1)::text) collate \"C\" < lower(($2)::text) collate \"C\" then -1 \
         when lower(($1)::text) = lower(($2)::text) then 0 else 1 end)",
    ),
    ("texticlike", 2, "(($1)::text ilike ($2)::text)"),
    ("texticnlike", 2, "(($1)::text not ilike ($2)::text)"),
    ("texticregexeq", 2, "(($1)::text ~* ($2)::text)"),
    ("texticregexne", 2, "(($1)::text !~* ($2)::text)"),
];

/// The SQL a citext-only function computes, when the extension is
/// installed (whether the call reaches it is the search path's question,
/// asked before planning).
pub fn citext_only_function(name: &str, args: usize) -> Option<&'static str> {
    if !crate::extension_installed("citext") {
        return None;
    }
    CITEXT_ONLY_FUNCTIONS
        .iter()
        .find(|(n, a, _)| *n == name && *a == args)
        .map(|(_, _, sql)| *sql)
}

/// Is `ty` the citext extension's type, wherever it is installed? Such a
/// value is text to every operator and function citext does not define.
pub fn is_citext_type(ty: &str) -> bool {
    let Some(schema) = crate::extension_schema("citext") else {
        return false;
    };
    let t = ty.trim();
    let t = t
        .strip_prefix("public.")
        .or_else(|| t.strip_prefix(&format!("{schema}.")))
        .unwrap_or(t);
    t.eq_ignore_ascii_case("citext")
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

/// Is `locale` the POSIX variant (`en-US-u-va-posix`)? ICU4C builds a
/// codepoint-order collator for it; PostgreSQL's `en-US-u-va-posix-x-icu`
/// orders that way too (measured 2026-10-18: ` `<`!`<`0`<`9`<`Z`<`a`,
/// `a0`<`aa`, `~` last), and codepoint order is what `Resolved::Bytes`
/// compares by.
fn posix_variant(locale: &str) -> bool {
    keyword(locale, "va").is_some_and(|v| v.eq_ignore_ascii_case("posix"))
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
            _ => {
                if posix_variant(&u.locale) {
                    return Ok(Resolved::Bytes);
                }
                Resolved::Icu {
                    locale: u.locale.clone(),
                    deterministic: u.deterministic,
                }
            }
        });
    }
    if let Some(locale) = icu_builtin(name) {
        if posix_variant(locale) {
            return Ok(Resolved::Bytes);
        }
        return Ok(Resolved::Icu {
            locale: locale.to_string(),
            deterministic: true,
        });
    }
    Err(does_not_exist(name))
}

/// PostgreSQL 15.19's ICU collations, as `pg_collation` lists them there:
/// see the file's header for the ones left out.
const ICU_COLLATIONS: &str = include_str!("pg_icu_collations.tsv");

/// The built-in ICU collations served here, as `(oid, name, ICU locale)`.
/// A `<tag>-x-icu` name is a collation only when it is one of these, as in
/// PostgreSQL, where `initdb` creates one per locale its ICU knows.
pub fn icu_collations() -> impl Iterator<Item = (i64, &'static str, &'static str)> {
    ICU_COLLATIONS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut p = l.split('\t');
            Some((p.next()?.parse().ok()?, p.next()?, p.next()?))
        })
}

/// The ICU locale of a built-in ICU collation.
fn icu_builtin(name: &str) -> Option<&'static str> {
    icu_collations()
        .find(|(_, n, _)| *n == name)
        .map(|(_, _, l)| l)
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

/// Language and script pairs that PostgreSQL's ICU orders as the language
/// alone. ICU4C has no tailoring for them, and its fallback drops the
/// script (`az-Cyrl` -> `az`). ICU4X falls back to the root instead. Each
/// pair was measured against PostgreSQL 15.19 (`az-Cyrl-x-icu` orders as
/// `az-x-icu`, not as `und-x-icu`).
const SCRIPT_ORDERS_AS_LANGUAGE: [(&str, &str); 5] = [
    ("az", "Cyrl"),
    ("hi", "Latn"),
    ("pa", "Arab"),
    ("uz", "Arab"),
    ("uz", "Cyrl"),
];

/// A collator over ICU4X's compiled data, or over the generated data of a
/// locale that data does not tailor (`icu_data`).
enum Built {
    Compiled(CollatorBorrowed<'static>),
    Generated(Box<Collator>),
    /// A collator with a literal rewrite applied before keying: the conjunct
    /// tailoring CLDR has for mr / kok / as (`&ह<क्ष`, `&হ<ক্ষ`) that ICU4X
    /// does not apply (RP-005). Each rule rewrites a conjunct to the anchor
    /// letter plus one U+FFFD per rule position: U+FFFD is a symbol, whose
    /// primary sorts above every script letter, so the conjunct lands
    /// immediately after the anchor yet still above anything the anchor
    /// prefixes (क्षा after हा, which a zero-width joiner would not give),
    /// and successive rules list their conjuncts in order.
    Rewritten(Box<Built>, &'static [(&'static str, &'static str)]),
}

/// The Devanagari conjuncts and where CLDR's mr/kok tailoring puts them:
/// `क्ष` after ह, then `ज्ञ` (measured on PostgreSQL 15.19, 2026-10-18).
const DEVANAGARI_CONJUNCTS: &[(&str, &str)] = &[("क्ष", "ह\u{fffd}"), ("ज्ञ", "ह\u{fffd}\u{fffd}")];
/// Bengali's single tailored conjunct (Assamese, `as`): `ক্ষ` after হ.
const BENGALI_CONJUNCTS: &[(&str, &str)] = &[("ক্ষ", "হ\u{fffd}")];

impl Built {
    fn write_sort_key_to(&self, text: &str, key: &mut Vec<u8>) {
        match self {
            Built::Compiled(c) => {
                let _ = c.write_sort_key_to(text, key);
            }
            Built::Generated(c) => {
                let _ = c.as_borrowed().write_sort_key_to(text, key);
            }
            Built::Rewritten(inner, rules) => {
                // The common string has no conjunct: no copy, no rewrite.
                if rules.iter().any(|(from, _)| text.contains(from)) {
                    let rewritten: String = rules
                        .iter()
                        .fold(text.to_string(), |s, (from, to)| s.replace(from, to));
                    inner.write_sort_key_to(&rewritten, key);
                } else {
                    inner.write_sort_key_to(text, key);
                }
            }
        }
    }
}

/// The conjunct tailoring for `parsed`'s locale, if CLDR has one that ICU4X
/// does not apply: mr and kok in Devanagari, as in Bengali.
fn conjunct_tailoring(
    parsed: &icu_locale::Locale,
) -> Option<&'static [(&'static str, &'static str)]> {
    let language = parsed.id.language.as_str().to_string();
    let script = parsed.id.script.map(|s| s.as_str().to_string());
    match (language.as_str(), script.as_deref()) {
        ("mr" | "kok", None | Some("Deva")) => Some(DEVANAGARI_CONJUNCTS),
        ("as", None | Some("Beng")) => Some(BENGALI_CONJUNCTS),
        _ => None,
    }
}

fn build(locale: &str) -> Option<Built> {
    let mut parsed: icu_locale::Locale = locale.parse().ok()?;
    // `kok-Latn` orders as the root on PostgreSQL (measured 2026-10-18);
    // ICU4X would build kok's Devanagari collation for the Latn script.
    if parsed.id.language.as_str() == "kok"
        && parsed.id.script.is_some_and(|s| s.as_str() == "Latn")
    {
        parsed = "und".parse().ok()?;
    }
    if let Some(script) = parsed.id.script {
        let language = parsed.id.language;
        if SCRIPT_ORDERS_AS_LANGUAGE
            .iter()
            .any(|(l, s)| language.as_str() == *l && script.as_str() == *s)
        {
            // ICU4C ends at the language (`pa-Arab-PK` -> `pa-Arab` -> `pa`),
            // so the region goes too: ICU4X would read `pa-PK` as
            // `pa-Arab-PK` and fall back to the root again.
            parsed.id.script = None;
            parsed.id.region = None;
        }
    }
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
    let built = if crate::icu_data::covers(parsed.id.language.as_str()) {
        Built::Generated(Box::new(crate::icu_data::collator(prefs, options)?))
    } else {
        Built::Compiled(Collator::try_new(prefs, options).ok()?)
    };
    // The conjunct rule is a property of the LOCALE, so it applies however
    // the collator was built.
    Some(match conjunct_tailoring(&parsed) {
        Some(rules) => Built::Rewritten(Box::new(built), rules),
        None => built,
    })
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
        cache[locale].write_sort_key_to(text, &mut key);
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
/// Every `COLLATE` in a statement names a collation that exists: 42704 at
/// the clause otherwise, as PostgreSQL's parse analysis reports it. Checked
/// once here because some paths that only ORDER by a collation (an
/// aggregate's `ORDER BY ... COLLATE`) fell back to no ordering at all.
pub(crate) fn check_clauses(stmt: &N) -> Result<()> {
    let check = |cc: &pg_query::protobuf::CollateClause| -> Result<()> {
        resolve(&clause_name(cc)).map(|_| ()).inspect_err(|_| {
            if cc.location >= 0 {
                crate::set_error_location(cc.location);
            }
        })
    };
    // Over a column the type comes first (`int_col COLLATE "x"` is 42804
    // `collations are not supported by type integer`), which only the
    // planner can tell; it checks those itself.
    let over_column = |cc: &pg_query::protobuf::CollateClause| {
        matches!(
            cc.arg.as_deref().and_then(|a| a.node.as_ref()),
            Some(N::ColumnRef(_))
        )
    };
    for (n, _, _, _) in stmt.nodes() {
        match n {
            pg_query::NodeRef::CollateClause(cc) if !over_column(cc) => check(cc)?,
            // The node walk does not enter an aggregate's own ORDER BY.
            pg_query::NodeRef::FuncCall(f) => {
                for o in &f.agg_order {
                    let Some(N::SortBy(sb)) = o.node.as_ref() else {
                        continue;
                    };
                    let Some(inner) = sb.node.as_deref().and_then(|x| x.node.as_ref()) else {
                        continue;
                    };
                    for (m, _, _, _) in inner.nodes() {
                        if let pg_query::NodeRef::CollateClause(cc) = m {
                            check(cc)?;
                        }
                    }
                    if let N::CollateClause(cc) = inner {
                        check(cc)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

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
                None => match icu_builtin(&source) {
                    Some(locale) => UserCollation {
                        name,
                        provider: 'i',
                        locale: locale.to_string(),
                        deterministic: true,
                        oid: 0,
                    },
                    None => return Err(does_not_exist(&source)),
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

/// citext's operators where the planner's own passes cannot see them, run
/// on a statement with its tables known (RP-178):
///
/// - the byte-order comparisons (`~<~` and the rest) arrive here as `<`
///   over operands `pattern_ops` wrapped in `COLLATE "C"` (location -1):
///   over a citext operand they compare the lowered strings, as
///   `citext_pattern_lt` does;
/// - `OPERATOR(s.=)` (or `<`, ...) named with citext's schema is citext's
///   comparison of the lowered strings whether or not the path reaches
///   `s`; named with `pg_catalog` it is text's.
pub fn citext_operators(node: &mut N, lookup: &dyn Fn(&str) -> Option<TableDef>) {
    let Some(schema) = crate::extension_schema("citext") else {
        return;
    };
    let tables: Vec<TableDef> = node
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::RangeVar(r) => lookup(&r.relname),
            _ => None,
        })
        .collect();
    let citext_operand = |n: &pg_query::protobuf::Node| -> bool {
        match n.node.as_ref() {
            Some(N::TypeCast(tc)) => tc.type_name.as_ref().is_some_and(|t| {
                t.array_bounds.is_empty() && is_citext_type(&crate::type_name_of(t))
            }),
            Some(N::ColumnRef(c)) => {
                let Some(N::String(name)) = c.fields.last().and_then(|f| f.node.as_ref()) else {
                    return false;
                };
                let mut types = tables
                    .iter()
                    .filter_map(|t| t.columns.iter().find(|col| col.name == name.sval))
                    .map(|col| is_citext_type(&col.pg_type));
                types.next().is_some_and(|first| first && types.all(|t| t))
            }
            _ => false,
        }
    };
    // SAFETY: the pointers `nodes_mut` hands out point into `node`, which
    // outlives the loop; each AExpr is changed in place, never replaced.
    unsafe {
        for (n, _, _) in node.nodes_mut() {
            let pg_query::NodeMut::AExpr(e) = n else {
                continue;
            };
            let e = &mut *e;
            let parts: Vec<String> = e
                .name
                .iter()
                .filter_map(|n| match n.node.as_ref()? {
                    N::String(s) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect();
            let comparison = |op: &str| matches!(op, "=" | "<>" | "<" | "<=" | ">" | ">=");
            match parts.as_slice() {
                // `pattern_ops`'s `COLLATE "C"` over a citext operand.
                [op] if comparison(op) => {
                    for side in [&mut e.lexpr, &mut e.rexpr] {
                        let Some(N::CollateClause(cc)) =
                            side.as_deref_mut().and_then(|n| n.node.as_mut())
                        else {
                            continue;
                        };
                        if cc.location != -1 {
                            continue;
                        }
                        if let Some(arg) = cc.arg.take() {
                            cc.arg = Some(Box::new(if citext_operand(&arg) {
                                lowered_fn(*arg)
                            } else {
                                *arg
                            }));
                        }
                    }
                }
                [s, op] if comparison(op) && (*s == schema || s == "pg_catalog") => {
                    let citext = [e.lexpr.as_deref(), e.rexpr.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(&citext_operand);
                    if !citext {
                        continue;
                    }
                    let to: fn(pg_query::protobuf::Node) -> pg_query::protobuf::Node =
                        if *s == schema { lowered_fn } else { as_text_fn };
                    for side in [&mut e.lexpr, &mut e.rexpr] {
                        if let Some(arg) = side.take() {
                            *side = Some(Box::new(to(*arg)));
                        }
                    }
                    e.name = vec![crate::string_node(op)];
                }
                _ => {}
            }
        }
    }
}

fn as_text_fn(arg: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(arg)),
            type_name: Some(crate::type_name_node("text")),
            location: -1,
        }))),
    }
}

fn lowered_fn(arg: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![crate::string_node("lower")],
            args: vec![as_text_fn(arg)],
            funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
            location: -1,
            ..Default::default()
        }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(locale: &str, words: &[&str]) -> String {
        let mut keyed: Vec<(Vec<u8>, &str)> = words
            .iter()
            .map(|w| (icu_key(locale, w).unwrap(), *w))
            .collect();
        keyed.sort();
        keyed.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(",")
    }

    /// PostgreSQL 15.19 (`ORDER BY w COLLATE "haw-x-icu"` and
    /// `"ln-CD-x-icu"`): tailorings ICU4X's compiled data lacks, from the
    /// generated data (`icu_data`).
    #[test]
    fn generated_tailorings_order_as_postgres() {
        assert_eq!(ordered("haw", &["a", "e", "b", "o"]), "a,e,o,b");
        assert_eq!(ordered("haw-US", &["a", "e", "b", "o"]), "a,e,o,b");
        assert_eq!(ordered("ln-CD", &["ə", "ɛ", "f"]), "ɛ,ə,f");
        assert_eq!(ordered("und", &["ə", "ɛ", "f"]), "ə,ɛ,f");
    }

    /// PostgreSQL 15.19 orders these script variants as their language
    /// (`ORDER BY w COLLATE "az-Cyrl-x-icu"` and so on), not as the root.
    #[test]
    fn script_variants_order_as_their_language() {
        let words = ["i", "ı", "h", "x", "q", "k", "ch", "c", "ç", "d"];
        let root = "c,ç,ch,d,h,i,ı,k,q,x";
        assert_eq!(ordered("und", &words), root);
        for az in ["az-Cyrl", "az-Cyrl-AZ"] {
            assert_eq!(ordered(az, &words), "c,ch,ç,d,h,x,ı,i,k,q", "{az}");
        }
        for uz in ["uz-Cyrl", "uz-Arab"] {
            assert_eq!(ordered(uz, &words), "c,ç,d,h,i,ı,k,q,x,ch", "{uz}");
        }
        for name in [
            "az-Cyrl-x-icu",
            "az-Cyrl-AZ-x-icu",
            "hi-Latn-IN-x-icu",
            "pa-Arab-PK-x-icu",
            "uz-Arab-AF-x-icu",
            "uz-Cyrl-UZ-x-icu",
        ] {
            assert!(icu_builtin(name).is_some(), "{name}");
        }
    }

    /// RP-005: the conjunct tailoring CLDR has for mr / kok / as that ICU4X
    /// does not apply, measured on PostgreSQL 15.19 (2026-10-18): `क्ष` and
    /// `ज्ञ` sort after `ह` (`क्ष` first); Assamese `ক্ষ` after `হ`.
    #[test]
    fn conjunct_tailorings_order_as_postgres() {
        assert_eq!(ordered("mr", &["क", "क्ष", "ख", "ह"]), "क,ख,ह,क्ष");
        assert_eq!(ordered("mr-IN", &["क", "ज्ञ", "क्ष", "ह"]), "क,ह,क्ष,ज्ञ");
        assert_eq!(ordered("kok", &["क", "क्ष", "ख", "ह"]), "क,ख,ह,क्ष");
        assert_eq!(ordered("kok-Deva-IN", &["क", "क्ष", "ख", "ह"]), "क,ख,ह,क्ष");
        assert_eq!(ordered("as", &["ক", "ক্ষ", "খ", "হ"]), "ক,খ,হ,ক্ষ");
        assert_eq!(ordered("as-IN", &["ক", "ক্ষ", "খ", "হ"]), "ক,খ,হ,ক্ষ");
        // The conjunct keeps its place above anything the anchor prefixes:
        // क्षा sorts after हा.
        assert_eq!(ordered("mr", &["क्षा", "हा"]), "हा,क्षा");
    }

    /// RP-005: `kok-Latn` orders as the root (measured on PostgreSQL 15.19),
    /// and `en-US-u-va-posix` — its `va` variant — by codepoints
    /// (` `<`!`<`0`<`9`<`Z`<`a`, `a0`<`aa`, `~` last), which is what
    /// `Resolved::Bytes` compares by.
    #[test]
    fn kok_latn_orders_as_root_and_posix_as_bytes() {
        let words = &["a", "A", "b", "z", "á", "ä", "1", " "];
        assert_eq!(ordered("kok-Latn", words), ordered("und", words));
        assert_eq!(ordered("kok-Latn-IN", words), ordered("und", words));
        let key = |t: &str| sort_key("en-US-u-va-posix-x-icu", t).unwrap();
        assert!(key("a0") < key("aa"), "'0' sorts before 'a'");
        assert!(key("Z") < key("a"), "uppercase before lowercase");
        assert!(key(" ") < key("!"), "space first");
        assert!(key("~") > key("z"), "'~' (0x7E) sorts after letters");
    }
}
