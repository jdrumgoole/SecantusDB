//! Collation: string comparison by language rules, as `mongod` does it.
//!
//! `mongod` hands every collated comparison to ICU 57.1, and so does this
//! module: `secantus-icu` builds mongo's own copy of ICU and its trimmed data
//! (`vendor/icu4c-57.1`, from mongo r8.2.11). Which locales exist, what each
//! option defaults to, how strings order and the bytes of a sort key are
//! therefore ICU's answers, reached through the calls mongod makes
//! (`collator_factory_icu.cpp`, `collator_interface_icu.cpp`).
//!
//! Three things live here:
//!
//! * [`Collation`] and the two parsers. [`parse_strict`] applies `mongod`'s
//!   own checks and answers with its codes and messages; [`parse`] is the
//!   lenient form for callers that have already validated.
//! * [`compare`] / [`equal`] and the sort key ([`sort_key`]), which is what an
//!   index entry or a group key holds for a string under a collation.
//! * The ACTIVE collation: a thread-local the aggregation and update engines
//!   set for the length of one evaluation ([`activate`]), read by
//!   `order::cmp`, the group key and the set operators. It is how
//!   `{$eq: ["a", "A"]}` sees the collation without every expression operator
//!   taking a parameter.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bson::{Bson, Document};
use secantus_icu::{reorder, value, Attribute, Collator, Opened};

/// The collator version `mongod` 8.2 reports and requires (its ICU).
pub const MONGOD_COLLATOR_VERSION: &str = secantus_icu::ICU_VERSION;

/// What an on-disk index entry's collation keys were made with. Bump it when
/// the collator changes the bytes of a sort key, and rebuild the indexes that
/// carry the older number. 1 was ICU4X's keys; 2 (2026-11-09) is ICU 57.1's,
/// mongod's own bytes.
pub const KEY_FORMAT: i32 = 2;

/// The locale ICU falls back to when it has no data for the one asked for;
/// mongod suggests no alternative then (`kFallbackLocaleName`).
const FALLBACK_LOCALE: &str = "root";

/// A parsed, usable collation. Never `simple`: that is `None` everywhere.
#[derive(Clone)]
pub struct Collation {
    collator: Arc<Collator>,
    /// The collation as `mongod` stores and echoes it: every option spelled
    /// out, in its order, with `version`.
    spec: Document,
}

impl std::fmt::Debug for Collation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Collation({})", self.spec)
    }
}

impl Collation {
    /// The full document `listIndexes` and `listCollections` show.
    pub fn spec(&self) -> &Document {
        &self.spec
    }

    /// Two collations are the same when their full specs are.
    pub fn same(&self, other: &Collation) -> bool {
        self.spec == other.spec
    }
}

/// A refused collation document: `mongod`'s code and message.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecError {
    pub code: i32,
    pub message: String,
}

fn err<T>(code: i32, message: String) -> Result<T, SpecError> {
    Err(SpecError { code, message })
}

/// How the caller's command names and prints a collation in its errors.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    /// The field path in a type error: `collation` for `find`,
    /// `create.collation` for `create`.
    pub path: &'a str,
    /// `create` prints the spec with every default filled in.
    pub print_defaults: bool,
}

impl Context<'static> {
    pub const COMMAND: Context<'static> = Context {
        path: "collation",
        print_defaults: false,
    };
    pub const CREATE: Context<'static> = Context {
        path: "create.collation",
        print_defaults: true,
    };
}

#[derive(Default)]
struct Fields {
    locale: Option<String>,
    strength: Option<i32>,
    case_level: Option<bool>,
    case_first: Option<String>,
    numeric_ordering: Option<bool>,
    alternate: Option<String>,
    max_variable: Option<String>,
    normalization: Option<bool>,
    backwards: Option<bool>,
    version: Option<String>,
}

fn type_name(v: &Bson) -> &'static str {
    crate::query::bson_type_name(v)
}

/// The first pass: types, unknown fields and the `strength` range, in the
/// order the document gives them.
fn read_fields(d: &Document, ctx: Context<'_>) -> Result<Fields, SpecError> {
    let path = ctx.path;
    let mut f = Fields::default();
    let wrong = |k: &str, v: &Bson, want: &str| -> SpecError {
        SpecError {
            code: 14,
            message: format!(
                "BSON field '{path}.{k}' is the wrong type '{}', expected type '{want}'",
                type_name(v)
            ),
        }
    };
    for (k, v) in d {
        let null = matches!(v, Bson::Null | Bson::Undefined);
        match k.as_str() {
            "locale" | "caseFirst" | "alternate" | "maxVariable" | "version" => {
                let s = match v {
                    _ if null => None,
                    Bson::String(s) => Some(s.clone()),
                    _ => return Err(wrong(k, v, "string")),
                };
                let allowed: &[&str] = match k.as_str() {
                    "caseFirst" => &["upper", "lower", "off"],
                    "alternate" => &["non-ignorable", "shifted"],
                    "maxVariable" => &["punct", "space"],
                    _ => &[],
                };
                if let Some(s) = &s {
                    if !allowed.is_empty() && !allowed.contains(&s.as_str()) {
                        return err(
                            2,
                            format!(
                                "Enumeration value '{s}' for field '{path}.{k}' is not a valid value."
                            ),
                        );
                    }
                }
                match k.as_str() {
                    "locale" => f.locale = s,
                    "caseFirst" => f.case_first = s,
                    "alternate" => f.alternate = s,
                    "maxVariable" => f.max_variable = s,
                    _ => f.version = s,
                }
            }
            "caseLevel" | "numericOrdering" | "normalization" => {
                let b = match v {
                    _ if null => None,
                    Bson::Boolean(b) => Some(*b),
                    _ => return Err(wrong(k, v, "bool")),
                };
                match k.as_str() {
                    "caseLevel" => f.case_level = b,
                    "numericOrdering" => f.numeric_ordering = b,
                    _ => f.normalization = b,
                }
            }
            "backwards" => match v {
                Bson::Boolean(b) => f.backwards = Some(*b),
                _ => {
                    return err(
                        14,
                        format!(
                            "Field 'backwards' should be a boolean value, but found: {}",
                            type_name(v)
                        ),
                    )
                }
            },
            "strength" => {
                // Truncated toward zero and clamped to an int32, as mongod's
                // "safe int" does; NaN is 0.
                let n: i64 = match v {
                    _ if null => continue,
                    Bson::Int32(n) => i64::from(*n),
                    Bson::Int64(n) => *n,
                    Bson::Double(x) if x.is_nan() => 0,
                    Bson::Double(x) => x.trunc().clamp(-2147483648.0, 2147483647.0) as i64,
                    Bson::Decimal128(x) => {
                        let x: f64 = x.to_string().parse().unwrap_or(f64::NAN);
                        if x.is_nan() {
                            0
                        } else {
                            x.trunc().clamp(-2147483648.0, 2147483647.0) as i64
                        }
                    }
                    _ => {
                        return err(
                            14,
                            format!(
                                "BSON field '{path}.strength' is the wrong type '{}', expected \
                                 types '[double, decimal, long, int]'",
                                type_name(v)
                            ),
                        )
                    }
                };
                let n = n.clamp(-2147483648, 2147483647);
                if n > 5 {
                    return err(
                        2,
                        format!("BSON field 'strength' value must be <= 5, actual value '{n}'"),
                    );
                }
                if n < 0 {
                    return err(
                        2,
                        format!("BSON field 'strength' value must be >= 0, actual value '{n}'"),
                    );
                }
                if n == 0 {
                    return err(
                        2,
                        "Enumeration value '0' for field 'collation.strength' is not a valid \
                         value."
                            .to_string(),
                    );
                }
                f.strength = Some(n as i32);
            }
            _ => {
                return err(
                    40415,
                    format!("BSON field '{path}.{k}' is an unknown field."),
                )
            }
        }
    }
    if f.locale.is_none() {
        return err(
            40414,
            format!("BSON field '{path}.locale' is missing but a required field"),
        );
    }
    Ok(f)
}

/// The spec as an error message prints it: as given, or (for `create`) with
/// the IDL's defaults filled in and `backwards` / `version` only when given.
fn printed(d: &Document, f: &Fields, ctx: Context<'_>) -> String {
    if !ctx.print_defaults {
        return crate::query::bson_value_repr(&Bson::Document(d.clone()));
    }
    let mut out = Document::new();
    out.insert("locale", f.locale.clone().unwrap_or_default());
    out.insert("caseLevel", f.case_level.unwrap_or(false));
    out.insert(
        "caseFirst",
        f.case_first.clone().unwrap_or_else(|| "off".to_string()),
    );
    out.insert("strength", f.strength.unwrap_or(3));
    out.insert("numericOrdering", f.numeric_ordering.unwrap_or(false));
    out.insert(
        "alternate",
        f.alternate
            .clone()
            .unwrap_or_else(|| "non-ignorable".to_string()),
    );
    out.insert(
        "maxVariable",
        f.max_variable
            .clone()
            .unwrap_or_else(|| "punct".to_string()),
    );
    out.insert("normalization", f.normalization.unwrap_or(false));
    if let Some(b) = f.backwards {
        out.insert("backwards", b);
    }
    if let Some(v) = &f.version {
        out.insert("version", v.clone());
    }
    crate::query::bson_value_repr(&Bson::Document(out))
}

fn on_off(b: bool) -> i32 {
    if b {
        value::ON
    } else {
        value::OFF
    }
}

fn icu_failed(what: &str, e: secantus_icu::Error, shown: &str) -> SpecError {
    SpecError {
        code: 96,
        message: format!("Failed to {what}: {e}. Collation spec: {shown}"),
    }
}

/// `updateCollationSpecFromICUCollator`: each option the document gives is
/// set on the collator, each it leaves out is read from it (the locale's own
/// default), and the result is the full spec in `mongod`'s order.
fn resolve(
    collator: &mut Collator,
    name: &str,
    f: &Fields,
    shown: &str,
) -> Result<Document, SpecError> {
    let fail = |field: &str, e| icu_failed(&format!("set '{field}' attribute"), e, shown);
    let read = |c: &Collator, attr, field: &str| {
        c.attribute(attr).map_err(|e| {
            icu_failed(
                &format!("get '{field}' attribute from icu::Collator"),
                e,
                shown,
            )
        })
    };
    let mut out = Document::new();
    out.insert("locale", name);
    let case_level = match f.case_level {
        Some(b) => {
            collator
                .set_attribute(Attribute::CaseLevel, on_off(b))
                .map_err(|e| fail("caseLevel", e))?;
            b
        }
        None => read(collator, Attribute::CaseLevel, "caseLevel")? == value::ON,
    };
    out.insert("caseLevel", case_level);
    let case_first = match &f.case_first {
        Some(cf) => {
            let v = match cf.as_str() {
                "upper" => value::UPPER_FIRST,
                "lower" => value::LOWER_FIRST,
                _ => value::OFF,
            };
            collator
                .set_attribute(Attribute::CaseFirst, v)
                .map_err(|e| fail("caseFirst", e))?;
            cf.clone()
        }
        None => match read(collator, Attribute::CaseFirst, "caseFirst")? {
            value::UPPER_FIRST => "upper",
            value::LOWER_FIRST => "lower",
            _ => "off",
        }
        .to_string(),
    };
    out.insert("caseFirst", case_first);
    let strength = match f.strength {
        Some(n) => {
            let v = match n {
                1 => value::PRIMARY,
                2 => value::SECONDARY,
                3 => value::TERTIARY,
                4 => value::QUATERNARY,
                _ => value::IDENTICAL,
            };
            collator
                .set_attribute(Attribute::Strength, v)
                .map_err(|e| fail("strength", e))?;
            n
        }
        None => match read(collator, Attribute::Strength, "strength")? {
            value::PRIMARY => 1,
            value::SECONDARY => 2,
            value::TERTIARY => 3,
            value::QUATERNARY => 4,
            _ => 5,
        },
    };
    out.insert("strength", strength);
    let numeric = match f.numeric_ordering {
        Some(b) => {
            collator
                .set_attribute(Attribute::NumericCollation, on_off(b))
                .map_err(|e| fail("numericOrdering", e))?;
            b
        }
        None => read(collator, Attribute::NumericCollation, "numericOrdering")? == value::ON,
    };
    out.insert("numericOrdering", numeric);
    let alternate = match &f.alternate {
        Some(a) => {
            let v = if a == "shifted" {
                value::SHIFTED
            } else {
                value::NON_IGNORABLE
            };
            collator
                .set_attribute(Attribute::AlternateHandling, v)
                .map_err(|e| fail("alternate", e))?;
            a.clone()
        }
        None => if read(collator, Attribute::AlternateHandling, "alternate")? == value::SHIFTED {
            "shifted"
        } else {
            "non-ignorable"
        }
        .to_string(),
    };
    out.insert("alternate", alternate);
    let max_variable = match &f.max_variable {
        Some(m) => {
            let v = if m == "space" {
                reorder::SPACE
            } else {
                reorder::PUNCTUATION
            };
            collator
                .set_max_variable(v)
                .map_err(|e| fail("maxVariable", e))?;
            m.clone()
        }
        None => if collator.max_variable() == reorder::SPACE {
            "space"
        } else {
            "punct"
        }
        .to_string(),
    };
    out.insert("maxVariable", max_variable);
    let normalization = match f.normalization {
        Some(b) => {
            collator
                .set_attribute(Attribute::NormalizationMode, on_off(b))
                .map_err(|e| fail("normalization", e))?;
            b
        }
        None => read(collator, Attribute::NormalizationMode, "normalization")? == value::ON,
    };
    out.insert("normalization", normalization);
    let backwards = match f.backwards {
        Some(b) => {
            collator
                .set_attribute(Attribute::FrenchCollation, on_off(b))
                .map_err(|e| fail("backwards", e))?;
            b
        }
        None => read(collator, Attribute::FrenchCollation, "backwards")? == value::ON,
    };
    out.insert("backwards", backwards);
    if let Some(v) = &f.version {
        if v != MONGOD_COLLATOR_VERSION {
            return err(
                161,
                format!(
                    "Requested collation version {v} but the only available collator version \
                     was {MONGOD_COLLATOR_VERSION}. Requested collation spec: {shown}"
                ),
            );
        }
    }
    out.insert("version", MONGOD_COLLATOR_VERSION);
    Ok(out)
}

/// `validateCollationSpec`, over the resolved options: a locale's own default
/// counts (`fr_CA` compares accents backwards unless told otherwise).
fn validate(spec: &Document, shown: &str) -> Result<(), SpecError> {
    let strength = spec.get_i32("strength").unwrap_or(3);
    if spec.get_bool("backwards") == Ok(true) && strength == 1 {
        return err(
            2,
            format!("'backwards' is invalid with 'strength' of 1 in: {shown}"),
        );
    }
    let case_first_on = spec.get_str("caseFirst").is_ok_and(|c| c != "off");
    if case_first_on && strength < 3 && spec.get_bool("caseLevel") != Ok(true) {
        return err(
            2,
            format!(
                "'caseFirst' is invalid unless 'caseLevel' is on or 'strength' is greater than 2 \
                 in: {shown}"
            ),
        );
    }
    Ok(())
}

/// One collator per distinct document: opening one reads locale data, and a
/// workload repeats the same few collations on every command. Only a
/// collation that parsed is kept; a refusal is worked out again.
fn cached(key: &str) -> Option<Collation> {
    cache().lock().ok().and_then(|c| c.get(key).cloned())
}

fn cache() -> &'static Mutex<HashMap<String, Collation>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Collation>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Parse a collation document with `mongod`'s checks, in its order
/// (`CollatorFactoryICU::makeFromBSON`). `Ok(None)` is the `simple` locale:
/// binary comparison, no collation.
///
/// An EMPTY document is the caller's to decide (`find` takes it as absent,
/// `create` refuses it); here it is a missing `locale`.
pub fn parse_strict(d: &Document, ctx: Context<'_>) -> Result<Option<Collation>, SpecError> {
    let mut f = read_fields(d, ctx)?;
    if ctx.print_defaults {
        // `create` hands ICU the IDL-parsed collation, every option but
        // `backwards` and `version` filled with its IDL default, so a locale's
        // own default counts only for those two there (mongod 8.2.11:
        // `{locale: "da"}` is `caseFirst: "off"` on a collection, "upper"
        // on an index).
        f.case_level.get_or_insert(false);
        f.case_first.get_or_insert_with(|| "off".to_string());
        f.strength.get_or_insert(3);
        f.numeric_ordering.get_or_insert(false);
        f.alternate
            .get_or_insert_with(|| "non-ignorable".to_string());
        f.max_variable.get_or_insert_with(|| "punct".to_string());
        f.normalization.get_or_insert(false);
    }
    let f = f;
    // `printed` fills the same IDL defaults for `create` and shows the
    // document as given otherwise, so the filling above does not show.
    let shown = || printed(d, &f, ctx);
    let locale = f.locale.clone().unwrap_or_default();
    if locale.contains('\0') {
        return err(
            2,
            format!(
                "Field 'locale' cannot contain null byte. Collation spec: {}",
                shown()
            ),
        );
    }
    if locale == "simple" {
        return Ok(None);
    }
    let key = format!(
        "{locale}\0{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        f.strength,
        f.case_level,
        f.case_first,
        f.numeric_ordering,
        f.alternate,
        f.max_variable,
        f.normalization,
        f.backwards,
        f.version
    );
    if let Some(found) = cached(&key) {
        return Ok(Some(found));
    }
    let shown = shown();
    let opened = Collator::open(&locale).map_err(|e| icu_failed("create collator", e, &shown))?;
    let (mut collator, name, valid) = match opened {
        Opened::Bogus => {
            return err(2, format!("Field 'locale' is not valid in: {shown}"));
        }
        Opened::Collator {
            collator,
            name,
            valid,
        } => (collator, name, valid),
    };
    if locale.is_empty() {
        return err(
            2,
            format!("Field 'locale' cannot be the empty string in: {shown}"),
        );
    }
    if locale != valid {
        let hint = if valid != FALLBACK_LOCALE && !valid.is_empty() {
            format!(". Did you mean '{valid}'?")
        } else {
            String::new()
        };
        return err(2, format!("Field 'locale' is invalid in: {shown}{hint}"));
    }
    let spec = resolve(&mut collator, &name, &f, &shown)?;
    validate(&spec, &shown)?;
    let made = Collation {
        collator: Arc::new(collator),
        spec,
    };
    if let Ok(mut c) = cache().lock() {
        c.insert(key, made.clone());
    }
    Ok(Some(made))
}

/// Parse a collation document that has already been validated, or that comes
/// from the catalog. Anything unusable is no collation. A document with
/// options and no `locale` is read as `en`: the engine-parity bindings send
/// `{strength, caseLevel}` alone.
pub fn parse(d: &Document) -> Option<Collation> {
    if d.is_empty() {
        return None;
    }
    if d.contains_key("locale") {
        return parse_strict(d, Context::COMMAND).ok().flatten();
    }
    let mut with_locale = d.clone();
    with_locale.insert("locale", "en");
    parse_strict(&with_locale, Context::COMMAND).ok().flatten()
}

/// Collation-aware string equality. `Some` always; the `Option` is the old
/// "defer" signal the callers still unwrap.
pub fn equal(a: &str, b: &str, c: &Collation) -> Option<bool> {
    Some(c.collator.compare(a, b) == Ordering::Equal)
}

/// Collation-aware string ordering.
pub fn compare(a: &str, b: &str, c: &Collation) -> Option<Ordering> {
    Some(c.collator.compare(a, b))
}

/// The string's sort key: bytes that order as the collation orders, and are
/// equal exactly when the collation calls two strings equal.
pub fn sort_key(s: &str, c: &Collation) -> Vec<u8> {
    c.collator.sort_key(s)
}

/// The bytes an index entry holds for a string under the collation.
pub fn normalize_index_bytes(s: &str, c: &Collation) -> Option<Vec<u8>> {
    Some(sort_key(s, c))
}

/// The bytes a sort compares for a string under the collation.
pub fn sort_level_bytes(s: &str, c: &Collation) -> Vec<u8> {
    sort_key(s, c)
}

thread_local! {
    static ACTIVE: RefCell<Option<Collation>> = const { RefCell::new(None) };
}

/// Restores the previously active collation when dropped.
pub struct ActiveGuard(Option<Collation>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.with(|a| *a.borrow_mut() = self.0.take());
    }
}

/// Make `c` the collation every value comparison on this thread uses until
/// the guard drops. `None` turns collation off for the same span, which is
/// what a nested evaluation with no collation needs.
#[must_use]
pub fn activate(c: Option<&Collation>) -> ActiveGuard {
    ACTIVE.with(|a| ActiveGuard(a.replace(c.cloned())))
}

/// The collation [`activate`] set, if any.
pub fn active() -> Option<Collation> {
    ACTIVE.with(|a| a.borrow().clone())
}

/// Compare two strings under the active collation, or by code point when
/// there is none.
/// [`active_compare`] over a string's bytes, which may not be UTF-8
/// (RM-222): byte order without a collation, as mongod compares strings;
/// under one, the collation over the text with each invalid sequence
/// replaced by U+FFFD (ICU's `compareUTF8` over the bytes, as mongod).
pub fn active_compare_bytes(a: &[u8], b: &[u8]) -> Ordering {
    ACTIVE.with(|c| match &*c.borrow() {
        Some(c) => c.collator.compare_bytes(a, b),
        None => a.cmp(b),
    })
}

pub fn active_compare(a: &str, b: &str) -> Ordering {
    ACTIVE.with(|c| match &*c.borrow() {
        Some(c) => c.collator.compare(a, b),
        None => a.cmp(b),
    })
}

/// `v` with every string (at any depth) replaced by its sort key under the
/// active collation, so that two values the collation calls equal have the
/// same bytes. The value itself when no collation is active. For keys that
/// are hashed rather than compared: a window or `$fill` partition.
pub fn fold_active(v: Bson) -> Bson {
    fn fold(v: Bson, c: &Collation) -> Bson {
        match v {
            Bson::String(s) => Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::UserDefined(0xC0),
                bytes: sort_key(&s, c),
            }),
            Bson::Array(a) => Bson::Array(a.into_iter().map(|x| fold(x, c)).collect()),
            Bson::Document(d) => {
                Bson::Document(d.into_iter().map(|(k, x)| (k, fold(x, c))).collect())
            }
            other => other,
        }
    }
    match active() {
        Some(c) => fold(v, &c),
        None => v,
    }
}

/// Whether a collation is active on this thread.
pub fn is_active() -> bool {
    ACTIVE.with(|a| a.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn c(d: Document) -> Collation {
        parse_strict(&d, Context::COMMAND).unwrap().unwrap()
    }

    fn refusal(d: Document) -> (i32, String) {
        let e = parse_strict(&d, Context::COMMAND).unwrap_err();
        (e.code, e.message)
    }

    #[test]
    fn strength_decides_what_is_equal() {
        let s1 = c(doc! {"locale": "en", "strength": 1});
        let s2 = c(doc! {"locale": "en", "strength": 2});
        let s3 = c(doc! {"locale": "en"});
        assert_eq!(equal("a", "Á", &s1), Some(true));
        assert_eq!(equal("a", "A", &s2), Some(true));
        assert_eq!(equal("a", "á", &s2), Some(false));
        assert_eq!(equal("a", "A", &s3), Some(false));
        assert_eq!(equal("ss", "ß", &s1), Some(true));
    }

    #[test]
    fn order_is_the_languages_not_the_code_points() {
        let en = c(doc! {"locale": "en"});
        // Code points put every capital before every small letter.
        assert_eq!(compare("a", "B", &en), Some(Ordering::Less));
        assert_eq!(compare("_a", "9", &en), Some(Ordering::Less));
        let numeric = c(doc! {"locale": "en", "numericOrdering": true});
        assert_eq!(compare("10", "9", &numeric), Some(Ordering::Greater));
        assert_eq!(compare("10", "9", &en), Some(Ordering::Less));
        let sv = c(doc! {"locale": "sv"});
        assert_eq!(compare("ä", "z", &sv), Some(Ordering::Greater));
        assert_eq!(compare("ä", "z", &en), Some(Ordering::Less));
    }

    #[test]
    fn backwards_compares_accents_from_the_end() {
        let fr = c(doc! {"locale": "fr"});
        let back = c(doc! {"locale": "fr", "backwards": true});
        let ca = c(doc! {"locale": "fr_CA"});
        let ca_off = c(doc! {"locale": "fr_CA", "backwards": false});
        assert_eq!(compare("coté", "côte", &fr), Some(Ordering::Less));
        assert_eq!(compare("coté", "côte", &back), Some(Ordering::Greater));
        assert_eq!(compare("coté", "côte", &ca), Some(Ordering::Greater));
        assert_eq!(compare("coté", "côte", &ca_off), Some(Ordering::Less));
        assert_eq!(ca.spec().get_bool("backwards"), Ok(true));
    }

    #[test]
    fn sort_keys_agree_with_compare() {
        let words = [
            "a", "A", "á", "b", "ab", "a b", "10", "9", "", "côte", "coté", "ss", "ß",
        ];
        for spec in [
            doc! {"locale": "en"},
            doc! {"locale": "en", "strength": 1},
            doc! {"locale": "en", "strength": 2, "caseLevel": true},
            doc! {"locale": "en", "alternate": "shifted", "strength": 4},
            doc! {"locale": "en", "numericOrdering": true, "caseFirst": "upper"},
            doc! {"locale": "fr_CA"},
            doc! {"locale": "de@collation=phonebook"},
        ] {
            let coll = c(spec.clone());
            for a in words {
                for b in words {
                    assert_eq!(
                        sort_key(a, &coll).cmp(&sort_key(b, &coll)),
                        compare(a, b, &coll).unwrap(),
                        "{spec} {a:?} {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_full_spec_is_mongods() {
        let got = c(doc! {"locale": "en", "strength": 2.9});
        assert_eq!(
            got.spec(),
            &doc! {"locale": "en", "caseLevel": false, "caseFirst": "off", "strength": 2,
            "numericOrdering": false, "alternate": "non-ignorable", "maxVariable": "punct",
            "normalization": false, "backwards": false, "version": "57.1"}
        );
        assert!(got.same(&c(
            doc! {"locale": "en", "strength": 2_i64, "caseLevel": false}
        )));
    }

    #[test]
    fn simple_is_no_collation_whatever_else_it_says() {
        let simple = |d| parse_strict(&d, Context::COMMAND).map(|c| c.is_none());
        assert_eq!(simple(doc! {"locale": "simple"}), Ok(true));
        assert_eq!(
            simple(doc! {"locale": "simple", "strength": 1, "version": "1"}),
            Ok(true)
        );
        assert_eq!(refusal(doc! {"locale": "simple", "strength": 9}).0, 2);
    }

    /// `mongod` 8.2.11's suggestions (2026-10-16): ICU reads a three-letter
    /// language or region as its two-letter code, and drops the variant after
    /// such a region. Bibliographic and deprecated codes are not mapped.
    #[test]
    fn did_you_mean_reads_three_letter_codes_as_icu_does() {
        let cases = [
            ("eng", Some("en")),
            ("ENG_usa", Some("en_US")),
            ("eng_US", Some("en_US")),
            ("en_USA", Some("en_US")),
            ("en_USA_POSIX", Some("en_US")),
            ("en_US_posix", Some("en_US_POSIX")),
            ("en_GBR", Some("en")),
            (
                "de_AUT@collation=phonebook",
                Some("de_AT@collation=phonebook"),
            ),
            ("deu@collation=phonebook", Some("de@collation=phonebook")),
            ("zho_Hant", Some("zh_Hant")),
            ("srp_Latn", Some("sr_Latn")),
            ("fra_CAN", Some("fr_CA")),
            ("spa_ES", Some("es")),
            ("ger", None),
            ("iw", None),
            ("und", None),
        ];
        for (locale, want) in cases {
            let (code, message) = refusal(doc! {"locale": locale});
            assert_eq!(code, 2, "{locale}");
            let got = message
                .split_once(". Did you mean '")
                .map(|(_, rest)| rest.trim_end_matches("'?"));
            assert_eq!(got, want, "{locale}: {message}");
        }
    }

    #[test]
    fn refusals_carry_mongods_code_and_message() {
        assert_eq!(
            refusal(doc! {"strength": 2}),
            (
                40414,
                "BSON field 'collation.locale' is missing but a required field".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": 5}),
            (
                14,
                "BSON field 'collation.locale' is the wrong type 'int', expected type 'string'"
                    .into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "zz_nope"}),
            (
                2,
                "Field 'locale' is invalid in: { locale: \"zz_nope\" }".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "EN"}),
            (
                2,
                "Field 'locale' is invalid in: { locale: \"EN\" }. Did you mean 'en'?".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "en-US"}).1,
            "Field 'locale' is invalid in: { locale: \"en-US\" }. Did you mean 'en_US'?"
        );
        assert_eq!(
            refusal(doc! {"locale": "de@collation=nope"}).1,
            "Field 'locale' is invalid in: { locale: \"de@collation=nope\" }. Did you mean 'de'?"
        );
        assert_eq!(
            refusal(doc! {"locale": "en", "strength": 6}),
            (
                2,
                "BSON field 'strength' value must be <= 5, actual value '6'".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "en", "strength": 0}).1,
            "Enumeration value '0' for field 'collation.strength' is not a valid value."
        );
        assert_eq!(
            refusal(doc! {"locale": "en", "backwards": 1}),
            (
                14,
                "Field 'backwards' should be a boolean value, but found: int".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "en", "strength": 1, "backwards": true}).1,
            "'backwards' is invalid with 'strength' of 1 in: { locale: \"en\", strength: 1, \
             backwards: true }"
        );
        assert_eq!(refusal(doc! {"locale": "en", "version": "1"}).0, 161);
        assert_eq!(
            refusal(doc! {"locale": "en", "bogus": 1}),
            (
                40415,
                "BSON field 'collation.bogus' is an unknown field.".into()
            )
        );
        assert_eq!(
            refusal(doc! {"locale": "en", "strength": 1, "caseFirst": "upper"}).1,
            "'caseFirst' is invalid unless 'caseLevel' is on or 'strength' is greater than 2 in: \
             { locale: \"en\", strength: 1, caseFirst: \"upper\" }"
        );
    }

    #[test]
    fn create_prints_the_defaults() {
        let e = parse_strict(&doc! {"locale": "EN"}, Context::CREATE).unwrap_err();
        assert_eq!(
            e.message,
            "Field 'locale' is invalid in: { locale: \"EN\", caseLevel: false, caseFirst: \
             \"off\", strength: 3, numericOrdering: false, alternate: \"non-ignorable\", \
             maxVariable: \"punct\", normalization: false }. Did you mean 'en'?"
        );
    }

    /// mongod 8.2.11 accepts each (RM-095): ICU 57's data has them, where
    /// ICU4X's did not. The spec echoes the locale as given.
    #[test]
    fn every_collation_type_mongods_data_has_is_accepted() {
        for locale in [
            "ko@collation=search",
            "ko@collation=searchjl",
            "zh@collation=big5han",
            "zh@collation=gb2312han",
            "ln@collation=phonetic",
            "en@collation=search",
        ] {
            assert_eq!(
                c(doc! {"locale": locale}).spec().get_str("locale"),
                Ok(locale)
            );
        }
    }

    /// RM-091: with `normalization: false` (the default) the two orderings
    /// of a dot below and a diaeresis on `o` are not equal, as on mongod.
    #[test]
    fn normalization_off_keeps_canonical_equivalents_apart() {
        let (a, b) = ("\u{1ECD}\u{308}", "\u{F6}\u{323}");
        let en = c(doc! {"locale": "en"});
        assert_eq!(equal(a, b, &en), Some(false));
        let on = c(doc! {"locale": "en", "normalization": true});
        assert_eq!(equal(a, b, &on), Some(true));
    }

    /// The sort key is mongod's: ICU's collation key less its trailing NUL
    /// (`"W\x01\x05"` for `"x"` under `en`, strength 2, in mongod 8.2.11's
    /// duplicate-key error).
    #[test]
    fn sort_keys_are_mongods_bytes() {
        let s2 = c(doc! {"locale": "en", "strength": 2});
        assert_eq!(sort_key("x", &s2), b"W\x01\x05");
        assert_eq!(sort_key("a", &s2), [0x29, 0x01, 0x05]);
    }

    #[test]
    fn the_active_collation_is_scoped() {
        assert_eq!(active_compare("a", "B"), Ordering::Greater);
        {
            let en = c(doc! {"locale": "en"});
            let _on = activate(Some(&en));
            assert_eq!(active_compare("a", "B"), Ordering::Less);
            {
                let _off = activate(None);
                assert_eq!(active_compare("a", "B"), Ordering::Greater);
            }
            assert!(is_active());
        }
        assert!(!is_active());
    }
}
