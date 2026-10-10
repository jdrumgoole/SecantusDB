//! Collation: string comparison by language rules, as `mongod` does it.
//!
//! `mongod` hands every collated comparison to ICU. This module hands it to
//! ICU4X (`icu_collator`), which implements the same algorithm (UCA with CLDR
//! tailorings) in Rust. Measured against `mongod` 8.2.11 on 2026-10-10 over
//! 157 strings and 40 collations (every option, 24 locales), the two put the
//! strings in the same order and the same equivalence classes, with one
//! exception recorded in `tasks/backlog.md`: ICU4X always treats canonically
//! equivalent strings as equal, where `mongod` with `normalization: false`
//! (the default) tells `"ö\u{323}"` from `"o\u{323}\u{308}"`.
//!
//! `mongod` 8.2 links ICU 57.1 and ICU4X carries newer CLDR data, so a
//! character added to Unicode since, or a tailoring CLDR has revised, can
//! order differently. Nothing here claims otherwise.
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
use icu_collator::options::{AlternateHandling, CaseLevel, CollatorOptions, MaxVariable, Strength};
use icu_collator::preferences::{CollationCaseFirst, CollationNumericOrdering};
use icu_collator::provider::{
    Baked, CollationDiacriticsV1, CollationJamoV1, CollationMetadata, CollationMetadataV1,
    CollationReorderingV1, CollationRootV1, CollationSpecialPrimariesV1, CollationTailoringV1,
};
use icu_collator::{Collator, CollatorPreferences};
use icu_normalizer::provider::{NormalizerNfdDataV1, NormalizerNfdTablesV1};
use icu_provider::prelude::*;

/// The collator version `mongod` 8.2 reports and requires (its ICU).
pub const MONGOD_COLLATOR_VERSION: &str = "57.1";

/// What an on-disk index entry's collation keys were made with. Bump it when
/// `icu_collator` or its data changes the bytes of a sort key, and rebuild the
/// indexes that carry the older number.
pub const KEY_FORMAT: i32 = 1;

/// Every locale `mongod` 8.2.11 accepts, found by asking it for each one
/// (all two- and three-letter languages, then each with every script and
/// region). `simple` is separate: it means no collation at all.
const LOCALES: &[&str] = &[
    "af",
    "am",
    "ar",
    "as",
    "az",
    "be",
    "bg",
    "bn",
    "bo",
    "bs",
    "bs_Cyrl",
    "ca",
    "chr",
    "cs",
    "cy",
    "da",
    "de",
    "de_AT",
    "dsb",
    "dz",
    "ee",
    "el",
    "en",
    "en_US",
    "en_US_POSIX",
    "eo",
    "es",
    "et",
    "fa",
    "fa_AF",
    "fi",
    "fil",
    "fo",
    "fr",
    "fr_CA",
    "ga",
    "gl",
    "gu",
    "ha",
    "haw",
    "he",
    "hi",
    "hr",
    "hsb",
    "hu",
    "hy",
    "id",
    "ig",
    "is",
    "it",
    "ja",
    "ka",
    "kk",
    "kl",
    "km",
    "kn",
    "ko",
    "kok",
    "ky",
    "lb",
    "lkt",
    "ln",
    "lo",
    "lt",
    "lv",
    "mk",
    "ml",
    "mn",
    "mr",
    "ms",
    "mt",
    "my",
    "nb",
    "ne",
    "nl",
    "nn",
    "om",
    "or",
    "pa",
    "pl",
    "ps",
    "pt",
    "ro",
    "ru",
    "se",
    "si",
    "sk",
    "sl",
    "smn",
    "sq",
    "sr",
    "sr_Latn",
    "sv",
    "sw",
    "ta",
    "te",
    "th",
    "to",
    "tr",
    "ug",
    "uk",
    "ur",
    "vi",
    "wae",
    "yi",
    "yo",
    "zh",
    "zh_Hant",
    "zu",
];

/// `@collation=<type>` values every locale takes, and the ones only some do
/// (measured the same way).
const COMMON_TYPES: &[&str] = &["search", "eor", "emoji"];
const LOCALE_TYPES: &[(&str, &[&str])] = &[
    ("ar", &["compat"]),
    ("bn", &["traditional"]),
    ("de", &["phonebook"]),
    ("de_AT", &["phonebook"]),
    ("es", &["traditional"]),
    ("fi", &["traditional"]),
    ("ja", &["unihan"]),
    ("kn", &["traditional"]),
    ("ko", &["unihan", "searchjl"]),
    ("ln", &["phonetic"]),
    ("si", &["dictionary"]),
    ("sv", &["standard"]),
    ("vi", &["traditional"]),
    (
        "zh",
        &[
            "standard",
            "stroke",
            "zhuyin",
            "big5han",
            "gb2312han",
            "unihan",
        ],
    ),
    (
        "zh_Hant",
        &[
            "standard",
            "pinyin",
            "zhuyin",
            "big5han",
            "gb2312han",
            "unihan",
        ],
    ),
];

/// Collation types `mongod` accepts that the bundled ICU4X data does not
/// carry. ICU4X would silently fall back to the locale's standard order, which
/// is a different answer, so these are refused by name.
const UNSUPPORTED_TYPES: &[&str] = &["search", "searchjl", "big5han", "gb2312han", "phonetic"];

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
/// the defaults filled in and `backwards` / `version` only when given.
fn printed(d: &Document, f: &Fields, ctx: Context<'_>) -> String {
    if !ctx.print_defaults {
        return crate::query::bson_value_repr(&Bson::Document(d.clone()));
    }
    let mut out = defaulted(f);
    out.remove("backwards");
    out.remove("version");
    if let Some(b) = f.backwards {
        out.insert("backwards", b);
    }
    if let Some(v) = &f.version {
        out.insert("version", v.clone());
    }
    crate::query::bson_value_repr(&Bson::Document(out))
}

/// Every option spelled out, in `mongod`'s order.
fn defaulted(f: &Fields) -> Document {
    let locale = f.locale.clone().unwrap_or_default();
    let mut out = Document::new();
    out.insert("locale", locale.clone());
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
    out.insert(
        "backwards",
        f.backwards.unwrap_or(locale_is_backwards(&locale)),
    );
    out.insert("version", MONGOD_COLLATOR_VERSION);
    out
}

/// Canadian French is the one locale whose accents compare from the end of
/// the string unless told otherwise.
fn locale_is_backwards(locale: &str) -> bool {
    locale.split('@').next() == Some("fr_CA")
}

fn types_for(base: &str) -> impl Iterator<Item = &'static str> {
    let own = LOCALE_TYPES
        .iter()
        .find(|(l, _)| *l == base)
        .map(|(_, t)| *t)
        .unwrap_or(&[]);
    COMMON_TYPES.iter().chain(own.iter()).copied()
}

/// Whether `mongod` accepts this locale string exactly as written.
fn locale_is_valid(locale: &str) -> bool {
    match locale.split_once('@') {
        None => LOCALES.contains(&locale),
        Some((base, keyword)) => {
            LOCALES.contains(&base)
                && keyword
                    .strip_prefix("collation=")
                    .is_some_and(|t| types_for(base).any(|x| x == t))
        }
    }
}

/// The valid locale nearest an invalid one, for `Did you mean`: the string in
/// ICU's casing, then with the keyword, variant, region and script dropped in
/// turn. `mongod` asks ICU for the functional equivalent, which also maps
/// three-letter codes (`eng`, `USA`) and knows `zh@collation=stroke` belongs
/// to `zh_Hant`; those it suggests and this does not.
fn nearest_locale(locale: &str) -> Option<String> {
    let (base, keyword) = match locale.split_once('@') {
        Some((b, k)) => (b, Some(k)),
        None => (locale, None),
    };
    let mut parts: Vec<String> = Vec::new();
    for (i, p) in base.split(['_', '-']).enumerate() {
        if p.is_empty() {
            break;
        }
        let alpha = p.chars().all(|c| c.is_ascii_alphabetic());
        parts.push(if i == 0 {
            p.to_ascii_lowercase()
        } else if p.len() == 4 && alpha {
            let mut s = p.to_ascii_lowercase();
            s[..1].make_ascii_uppercase();
            s
        } else {
            p.to_ascii_uppercase()
        });
    }
    if let Some(k) = keyword {
        let whole = format!("{}@{k}", parts.join("_"));
        if locale_is_valid(&whole) {
            return Some(whole);
        }
    }
    while !parts.is_empty() {
        let candidate = parts.join("_");
        if LOCALES.contains(&candidate.as_str()) {
            return Some(candidate);
        }
        parts.pop();
    }
    None
}

/// `mongod`'s locale name as the BCP 47 tag ICU4X takes.
fn bcp47(locale: &str) -> Result<String, SpecError> {
    let (base, keyword) = match locale.split_once('@') {
        Some((b, k)) => (b, k.strip_prefix("collation=")),
        None => (locale, None),
    };
    let mut tag = base.replace('_', "-").replace("-POSIX", "-posix");
    let co = match (base, keyword) {
        (_, Some(t)) if UNSUPPORTED_TYPES.contains(&t) => {
            return err(
                2,
                format!(
                    "collation type '{t}' in locale \"{locale}\" is not supported by this server"
                ),
            )
        }
        // ICU 57's Swedish default is the reformed order and its `standard`
        // the older one (v and w alike); CLDR has since renamed them.
        ("sv", Some("standard")) => Some("trad"),
        (_, Some("standard")) | (_, None) => None,
        (_, Some("phonebook")) => Some("phonebk"),
        (_, Some("traditional")) => Some("trad"),
        (_, Some("dictionary")) => Some("dict"),
        (_, Some(t)) => Some(t),
    };
    if let Some(co) = co {
        tag.push_str("-u-co-");
        tag.push_str(co);
    }
    Ok(tag)
}

/// ICU4X decides the French accent order from locale data alone. This
/// provider hands the collator its normal data with that one bit set as the
/// collation document asks.
struct WithBackwards(bool);

macro_rules! delegate {
    ($provider:path => $($marker:ty),*) => {$(
        impl DataProvider<$marker> for WithBackwards {
            fn load(&self, req: DataRequest) -> Result<DataResponse<$marker>, DataError> {
                DataProvider::<$marker>::load(&$provider, req)
            }
        }
    )*};
}
delegate!(Baked => CollationSpecialPrimariesV1, CollationRootV1, CollationTailoringV1,
    CollationDiacriticsV1, CollationJamoV1, CollationReorderingV1);
delegate!(icu_normalizer::provider::Baked => NormalizerNfdDataV1, NormalizerNfdTablesV1);

impl DataProvider<CollationMetadataV1> for WithBackwards {
    fn load(&self, req: DataRequest) -> Result<DataResponse<CollationMetadataV1>, DataError> {
        /// `CollationMetadata`'s backward-second-level bit.
        const BACKWARD_SECOND_LEVEL: u32 = 1 << 7;
        let found = DataProvider::<CollationMetadataV1>::load(&Baked, req)?;
        let mut metadata: CollationMetadata = *found.payload.get();
        if self.0 {
            metadata.bits |= BACKWARD_SECOND_LEVEL;
        } else {
            metadata.bits &= !BACKWARD_SECOND_LEVEL;
        }
        Ok(DataResponse {
            metadata: found.metadata,
            payload: DataPayload::from_owned(metadata),
        })
    }
}

fn build(full: &Document) -> Result<Collator, SpecError> {
    let text = |k: &str| full.get_str(k).unwrap_or_default();
    let flag = |k: &str| full.get_bool(k).unwrap_or(false);
    let locale = text("locale");
    let tag = bcp47(locale)?;
    let parsed: icu_locale_core::Locale = tag.parse().map_err(|_| SpecError {
        code: 2,
        message: format!("Field 'locale' is invalid in: {{ locale: \"{locale}\" }}"),
    })?;
    let mut prefs = CollatorPreferences::from(&parsed);
    prefs.numeric_ordering = Some(if flag("numericOrdering") {
        CollationNumericOrdering::True
    } else {
        CollationNumericOrdering::False
    });
    prefs.case_first = Some(match text("caseFirst") {
        "upper" => CollationCaseFirst::Upper,
        "lower" => CollationCaseFirst::Lower,
        _ => CollationCaseFirst::False,
    });
    let mut options = CollatorOptions::default();
    options.strength = Some(match full.get_i32("strength").unwrap_or(3) {
        1 => Strength::Primary,
        2 => Strength::Secondary,
        3 => Strength::Tertiary,
        4 => Strength::Quaternary,
        _ => Strength::Identical,
    });
    options.case_level = Some(if flag("caseLevel") {
        CaseLevel::On
    } else {
        CaseLevel::Off
    });
    options.alternate_handling = Some(if text("alternate") == "shifted" {
        AlternateHandling::Shifted
    } else {
        AlternateHandling::NonIgnorable
    });
    options.max_variable = Some(if text("maxVariable") == "space" {
        MaxVariable::Space
    } else {
        MaxVariable::Punctuation
    });
    Collator::try_new_unstable(&WithBackwards(flag("backwards")), prefs, options).map_err(|e| {
        SpecError {
            code: 2,
            message: format!("collation for locale \"{locale}\" could not be loaded: {e}"),
        }
    })
}

/// One collator per distinct spec: building one reads locale data, and a
/// workload repeats the same few collations on every command.
fn collator_for(full: &Document) -> Result<Arc<Collator>, SpecError> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Collator>>>> = OnceLock::new();
    let key = full.to_string();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(found);
    }
    let built = Arc::new(build(full)?);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, built.clone());
    }
    Ok(built)
}

/// Parse a collation document with `mongod`'s checks. `Ok(None)` is the
/// `simple` locale: binary comparison, no collation.
///
/// An EMPTY document is the caller's to decide (`find` takes it as absent,
/// `create` refuses it); here it is a missing `locale`.
pub fn parse_strict(d: &Document, ctx: Context<'_>) -> Result<Option<Collation>, SpecError> {
    let f = read_fields(d, ctx)?;
    let locale = f.locale.clone().unwrap_or_default();
    if locale == "simple" {
        return Ok(None);
    }
    let shown = || printed(d, &f, ctx);
    if locale.contains('\0') {
        return err(
            2,
            format!(
                "Field 'locale' cannot contain null byte. Collation spec: {}",
                shown()
            ),
        );
    }
    if locale.is_empty() {
        return err(
            2,
            format!("Field 'locale' cannot be the empty string in: {}", shown()),
        );
    }
    if !locale_is_valid(&locale) {
        let hint = match nearest_locale(&locale) {
            Some(n) if n != locale => format!(". Did you mean '{n}'?"),
            _ => String::new(),
        };
        return err(
            2,
            format!("Field 'locale' is invalid in: {}{hint}", shown()),
        );
    }
    if let Some(v) = &f.version {
        if v != MONGOD_COLLATOR_VERSION {
            return err(
                161,
                format!(
                    "Requested collation version {v} but the only available collator version \
                     was {MONGOD_COLLATOR_VERSION}. Requested collation spec: {}",
                    shown()
                ),
            );
        }
    }
    let strength = f.strength.unwrap_or(3);
    let case_first_on = f.case_first.as_deref().is_some_and(|c| c != "off");
    if case_first_on && strength < 3 && !f.case_level.unwrap_or(false) {
        return err(
            2,
            format!(
                "'caseFirst' is invalid unless 'caseLevel' is on or 'strength' is greater than 2 \
                 in: {}",
                shown()
            ),
        );
    }
    if f.backwards == Some(true) && strength == 1 {
        return err(
            2,
            format!(
                "'backwards' is invalid with 'strength' of 1 in: {}",
                shown()
            ),
        );
    }
    let spec = defaulted(&f);
    let collator = collator_for(&spec)?;
    Ok(Some(Collation { collator, spec }))
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
    Some(c.collator.as_borrowed().compare(a, b) == Ordering::Equal)
}

/// Collation-aware string ordering.
pub fn compare(a: &str, b: &str, c: &Collation) -> Option<Ordering> {
    Some(c.collator.as_borrowed().compare(a, b))
}

/// The string's sort key: bytes that order as the collation orders, and are
/// equal exactly when the collation calls two strings equal.
pub fn sort_key(s: &str, c: &Collation) -> Vec<u8> {
    let mut key = Vec::with_capacity(s.len() * 2 + 4);
    let Ok(()) = c.collator.as_borrowed().write_sort_key_to(s, &mut key);
    key
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
pub fn active_compare(a: &str, b: &str) -> Ordering {
    ACTIVE.with(|c| match &*c.borrow() {
        Some(c) => c.collator.as_borrowed().compare(a, b),
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

    #[test]
    fn a_type_the_data_lacks_is_refused_not_approximated() {
        let (code, message) = refusal(doc! {"locale": "en@collation=search"});
        assert_eq!(code, 2);
        assert!(
            message.contains("not supported by this server"),
            "{message}"
        );
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
