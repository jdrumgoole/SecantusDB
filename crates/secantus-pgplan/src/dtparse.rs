//! PostgreSQL's general date/time INPUT parser: `ParseDateTime` and
//! `DecodeDateTime` from `datetime.c`, transcribed.
//!
//! The strict ISO shapes (`2020-01-05`, `2020-01-05 10:00:00+02`) are parsed
//! on the fast paths in `lib.rs`; this is what reads everything else a
//! PostgreSQL client may send -- `Jan 5, 2020`, `5 January 2020`,
//! `1/5/2020` (in the session's DateStyle order), `20200105`, `2020.005`,
//! `J2458854`, `January 5 2020 10:00 PM EST`, `2020-01-05T10:00:00Z`,
//! `yesterday`, ISO `y2020m01d05`, and the rest.
//!
//! The field masks and the order in which fields are tried are PostgreSQL's,
//! because that order IS the semantics: whether `02-03-04` is a year, a
//! month or a day depends on what was seen before it.

use crate::{Error, Result};

// Field kinds from ParseDateTime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ftype {
    Number,
    String,
    Date,
    Time,
    Tz,
    Special,
}

// DTK_M bits (datetime.h's field numbers).
const YEAR: u32 = 1 << 2;
const MONTH: u32 = 1 << 1;
const DAY: u32 = 1 << 3;
const HOUR: u32 = 1 << 10;
const MINUTE: u32 = 1 << 11;
const SECOND: u32 = 1 << 12;
const MILLISECOND: u32 = 1 << 13;
const MICROSECOND: u32 = 1 << 14;
const TZ: u32 = 1 << 5;
const DOY: u32 = 1 << 15;
const DTZMOD: u32 = 1 << 6;
const DATE_M: u32 = YEAR | MONTH | DAY;
const TIME_M: u32 = HOUR | MINUTE | SECOND | MILLISECOND | MICROSECOND;
const ALL_SECS_M: u32 = SECOND | MILLISECOND | MICROSECOND;

/// The session's DateStyle field order, which decides `1/5/2020`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateOrder {
    Ymd,
    #[default]
    Mdy,
    Dmy,
}

thread_local! {
    static DATE_ORDER: std::cell::Cell<DateOrder> = const { std::cell::Cell::new(DateOrder::Mdy) };
}

/// Install the session's DateStyle order for the statements that follow.
pub fn set_date_order(order: DateOrder) {
    DATE_ORDER.with(|o| o.set(order));
}

fn date_order() -> DateOrder {
    DATE_ORDER.with(|o| o.get())
}

/// The reserved words that are a VALUE rather than fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Epoch,
    Infinity,
    NegInfinity,
    Now,
}

/// A decoded date/time. `year` is astronomical (1 BC is 0).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micros: i64,
    /// The offset EAST of UTC in seconds, when the input named one.
    pub offset: Option<i32>,
    /// A full zone name (`America/New_York`), resolved by the caller.
    pub zone: Option<String>,
    pub special: Option<Special>,
    /// Whether a date part was present (a bare time is not a date).
    pub has_date: bool,
    /// `today` / `yesterday` / `tomorrow`: days from the current date.
    pub relative_day: Option<i64>,
}

enum Fail {
    BadFormat,
    FieldOverflow,
    MdFieldOverflow,
    TzOverflow,
}

/// Error text for `type`, as `DateTimeParseError` words it.
fn to_error(f: Fail, text: &str, type_name: &str) -> Error {
    match f {
        Fail::BadFormat => Error::InvalidDatetimeFormat(format!(
            "invalid input syntax for type {type_name}: \"{text}\""
        )),
        Fail::FieldOverflow => {
            Error::DatetimeFieldOverflow(format!("date/time field value out of range: \"{text}\""))
        }
        // A month or day out of range may be a DateStyle mix-up.
        Fail::MdFieldOverflow => Error::DatetimeFieldOverflow(format!(
            "date/time field value out of range: \"{text}\"\nHint: Perhaps you need a different \"datestyle\" setting."
        )),
        Fail::TzOverflow => Error::Sqlstate(
            "22009",
            format!("time zone displacement out of range: \"{text}\""),
        ),
    }
}

fn parse_fields(text: &str) -> std::result::Result<Vec<(String, Ftype)>, Fail> {
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let lower = |c: char| c.to_ascii_lowercase();
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let mut f = String::new();
        let kind;
        if c.is_ascii_digit() {
            while i < cs.len() && cs[i].is_ascii_digit() {
                f.push(cs[i]);
                i += 1;
            }
            if i < cs.len() && cs[i] == ':' {
                kind = Ftype::Time;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == ':' || cs[i] == '.') {
                    f.push(cs[i]);
                    i += 1;
                }
            } else if i < cs.len() && matches!(cs[i], '-' | '/' | '.') {
                let delim = cs[i];
                f.push(delim);
                i += 1;
                if i < cs.len() && cs[i].is_ascii_digit() {
                    let mut k = if delim == '.' {
                        Ftype::Number
                    } else {
                        Ftype::Date
                    };
                    while i < cs.len() && cs[i].is_ascii_digit() {
                        f.push(cs[i]);
                        i += 1;
                    }
                    if i < cs.len() && cs[i] == delim {
                        k = Ftype::Date;
                        f.push(cs[i]);
                        i += 1;
                        while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == delim) {
                            f.push(cs[i]);
                            i += 1;
                        }
                    }
                    kind = k;
                } else {
                    kind = Ftype::Date;
                    while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == delim) {
                        f.push(lower(cs[i]));
                        i += 1;
                    }
                }
            } else {
                kind = Ftype::Number;
            }
        } else if c == '.' {
            f.push(c);
            i += 1;
            while i < cs.len() && cs[i].is_ascii_digit() {
                f.push(cs[i]);
                i += 1;
            }
            kind = Ftype::Number;
        } else if c.is_ascii_alphabetic() {
            let mut k = Ftype::String;
            while i < cs.len() && cs[i].is_ascii_alphabetic() {
                f.push(lower(cs[i]));
                i += 1;
            }
            let is_date = if i < cs.len() && matches!(cs[i], '-' | '/' | '.') {
                true
            } else if i < cs.len() && (cs[i] == '+' || cs[i].is_ascii_digit()) {
                keyword(&f).is_none()
            } else {
                false
            };
            if is_date {
                k = Ftype::Date;
                loop {
                    f.push(lower(cs[i]));
                    i += 1;
                    if !(i < cs.len()
                        && (matches!(cs[i], '+' | '-' | '/' | '_' | '.' | ':')
                            || cs[i].is_ascii_alphanumeric()))
                    {
                        break;
                    }
                }
            }
            kind = k;
        } else if c == '+' || c == '-' {
            f.push(c);
            i += 1;
            while i < cs.len() && cs[i].is_whitespace() {
                i += 1;
            }
            if i < cs.len() && cs[i].is_ascii_digit() {
                kind = Ftype::Tz;
                while i < cs.len() && (cs[i].is_ascii_digit() || matches!(cs[i], ':' | '.' | '-')) {
                    f.push(cs[i]);
                    i += 1;
                }
            } else if i < cs.len() && cs[i].is_ascii_alphabetic() {
                kind = Ftype::Special;
                while i < cs.len() && cs[i].is_ascii_alphabetic() {
                    f.push(lower(cs[i]));
                    i += 1;
                }
            } else {
                return Err(Fail::BadFormat);
            }
        } else if c.is_ascii_punctuation() {
            i += 1;
            continue;
        } else {
            return Err(Fail::BadFormat);
        }
        out.push((f, kind));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Month(u32),
    Dow,
    AmPm(bool),
    AdBc(bool),
    Ignore,
    Units(u32),
    IsoTime,
    DtzMod(i32),
    Reserved(Res),
    Tz(i32),
    Dtz(i32),
    /// A dynamic abbreviation (`MSK`): an index into `DYNAMIC`, resolved
    /// against its zone's history once the date is known.
    DynTz(u16),
}

/// The `Default` set's DYNAMIC abbreviations: each names a zone rather than
/// an offset, and means that zone's offset at the time given
/// (`timezonesets/Default`, PostgreSQL 15).
const DYNAMIC: &[(&str, &str)] = &[
    ("art", "America/Argentina/Buenos_Aires"),
    ("arst", "America/Argentina/Buenos_Aires"),
    ("clt", "America/Santiago"),
    ("gyt", "America/Guyana"),
    ("pyt", "America/Asuncion"),
    ("vet", "America/Caracas"),
    ("davt", "Antarctica/Davis"),
    ("mawt", "Antarctica/Mawson"),
    ("amst", "Asia/Yerevan"),
    ("anast", "Asia/Anadyr"),
    ("anat", "Asia/Anadyr"),
    ("azst", "Asia/Baku"),
    ("azt", "Asia/Baku"),
    ("gest", "Asia/Tbilisi"),
    ("get", "Asia/Tbilisi"),
    ("irkst", "Asia/Irkutsk"),
    ("irkt", "Asia/Irkutsk"),
    ("kgt", "Asia/Bishkek"),
    ("krast", "Asia/Krasnoyarsk"),
    ("krat", "Asia/Krasnoyarsk"),
    ("lkt", "Asia/Colombo"),
    ("magst", "Asia/Magadan"),
    ("magt", "Asia/Magadan"),
    ("novst", "Asia/Novosibirsk"),
    ("novt", "Asia/Novosibirsk"),
    ("omsst", "Asia/Omsk"),
    ("omst", "Asia/Omsk"),
    ("petst", "Asia/Kamchatka"),
    ("pett", "Asia/Kamchatka"),
    ("sgt", "Asia/Singapore"),
    ("tmt", "Asia/Ashgabat"),
    ("ulat", "Asia/Ulaanbaatar"),
    ("vlast", "Asia/Vladivostok"),
    ("vlat", "Asia/Vladivostok"),
    ("yakst", "Asia/Yakutsk"),
    ("yakt", "Asia/Yakutsk"),
    ("yekt", "Asia/Yekaterinburg"),
    ("fkst", "Atlantic/Stanley"),
    ("fkt", "Atlantic/Stanley"),
    ("lhdt", "Australia/Lord_Howe"),
    ("msk", "Europe/Moscow"),
    ("volt", "Europe/Volgograd"),
    ("iot", "Indian/Chagos"),
    ("ckt", "Pacific/Rarotonga"),
    ("easst", "Pacific/Easter"),
    ("east", "Pacific/Easter"),
    ("kost", "Pacific/Kosrae"),
    ("lint", "Pacific/Kiritimati"),
    ("nut", "Pacific/Niue"),
    ("tkt", "Pacific/Fakaofo"),
];

/// A dynamic abbreviation's offset at a local date and time
/// (`DetermineTimeZoneAbbrevOffset`): the zone's offset then when it went
/// by this abbreviation, else its standard (or, for a daylight-saving
/// abbreviation, its daylight) offset then.
fn dynamic_offset(i: u16, y: i32, mo: u32, d: u32, h: u32, mi: u32, sec: u32) -> Option<i32> {
    use chrono::{NaiveDate, Offset, TimeZone};
    use chrono_tz::{OffsetComponents, OffsetName};
    let (abbrev, zone) = DYNAMIC.get(usize::from(i))?;
    let tz: chrono_tz::Tz = zone.parse().ok()?;
    let local = NaiveDate::from_ymd_opt(y, mo, d)?.and_hms_opt(h.min(23), mi, sec.min(59))?;
    let off = tz
        .offset_from_local_datetime(&local)
        .earliest()
        .unwrap_or_else(|| tz.offset_from_utc_datetime(&local));
    // The zone went by this abbreviation then -- or tzdata now names that
    // period by its number (`+10`), which says nothing against it.
    let named = off.abbreviation();
    if named.is_none_or(|a| a.eq_ignore_ascii_case(abbrev) || a.starts_with(['+', '-'])) {
        return Some(off.fix().local_minus_utc());
    }
    let base = off.base_utc_offset().num_seconds() as i32;
    let dst = default_abbreviation(abbrev).is_some_and(|(_, dst)| dst);
    Some(if dst { base + 3600 } else { base })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Res {
    Epoch,
    Late,
    Early,
    Now,
    Today,
    Tomorrow,
    Yesterday,
    Zulu,
}

// UNITS values used by the ISO labelled form.
const U_YEAR: u32 = 1;
const U_MONTH: u32 = 2;
const U_DAY: u32 = 3;
const U_HOUR: u32 = 4;
const U_MINUTE: u32 = 5;
const U_SECOND: u32 = 6;
const U_JULIAN: u32 = 7;
const U_TIME: u32 = 8;

fn keyword(t: &str) -> Option<Tok> {
    Some(match t {
        "-infinity" => Tok::Reserved(Res::Early),
        "ad" => Tok::AdBc(false),
        "allballs" => Tok::Reserved(Res::Zulu),
        "am" => Tok::AmPm(false),
        "apr" | "april" => Tok::Month(4),
        "at" | "on" => Tok::Ignore,
        "aug" | "august" => Tok::Month(8),
        "bc" => Tok::AdBc(true),
        "d" => Tok::Units(U_DAY),
        "dec" | "december" => Tok::Month(12),
        "dst" => Tok::DtzMod(3600),
        "epoch" => Tok::Reserved(Res::Epoch),
        "feb" | "february" => Tok::Month(2),
        "fri" | "friday" | "mon" | "monday" | "sat" | "saturday" | "sun" | "sunday" | "thu"
        | "thur" | "thurs" | "thursday" | "tue" | "tues" | "tuesday" | "wed" | "wednesday"
        | "weds" => Tok::Dow,
        "h" => Tok::Units(U_HOUR),
        "infinity" | "+infinity" => Tok::Reserved(Res::Late),
        "j" | "jd" | "julian" => Tok::Units(U_JULIAN),
        "jan" | "january" => Tok::Month(1),
        "jul" | "july" => Tok::Month(7),
        "jun" | "june" => Tok::Month(6),
        "m" => Tok::Units(U_MONTH),
        "mar" | "march" => Tok::Month(3),
        "may" => Tok::Month(5),
        "mm" => Tok::Units(U_MINUTE),
        "nov" | "november" => Tok::Month(11),
        "now" => Tok::Reserved(Res::Now),
        "oct" | "october" => Tok::Month(10),
        "pm" => Tok::AmPm(true),
        "s" => Tok::Units(U_SECOND),
        "sep" | "sept" | "september" => Tok::Month(9),
        "t" => Tok::IsoTime,
        "today" => Tok::Reserved(Res::Today),
        "tomorrow" => Tok::Reserved(Res::Tomorrow),
        "y" => Tok::Units(U_YEAR),
        "yesterday" => Tok::Reserved(Res::Yesterday),
        // The time zone abbreviations of PostgreSQL's `Default` set that are
        // fixed offsets (seconds EAST); DST ones are `Dtz`.
        "utc" | "gmt" | "ut" | "z" | "zulu" => Tok::Tz(0),
        "est" => Tok::Tz(-5 * 3600),
        "edt" => Tok::Dtz(-4 * 3600),
        "cst" => Tok::Tz(-6 * 3600),
        "cdt" => Tok::Dtz(-5 * 3600),
        "mst" => Tok::Tz(-7 * 3600),
        "mdt" => Tok::Dtz(-6 * 3600),
        "pst" => Tok::Tz(-8 * 3600),
        "pdt" => Tok::Dtz(-7 * 3600),
        "akst" => Tok::Tz(-9 * 3600),
        "akdt" => Tok::Dtz(-8 * 3600),
        "hst" => Tok::Tz(-10 * 3600),
        "cet" => Tok::Tz(3600),
        "cest" => Tok::Dtz(2 * 3600),
        "met" => Tok::Tz(3600),
        "mest" => Tok::Dtz(2 * 3600),
        "eet" => Tok::Tz(2 * 3600),
        "eest" => Tok::Dtz(3 * 3600),
        "wet" => Tok::Tz(0),
        "west" => Tok::Dtz(3600),
        "jst" => Tok::Tz(9 * 3600),
        "kst" => Tok::Tz(9 * 3600),
        "ist" => Tok::Tz(2 * 3600),
        "aest" => Tok::Tz(10 * 3600),
        "aedt" => Tok::Dtz(11 * 3600),
        "nzst" => Tok::Tz(12 * 3600),
        "nzdt" => Tok::Dtz(13 * 3600),
        _ => {
            if let Some(i) = DYNAMIC.iter().position(|(a, _)| *a == t) {
                return Some(Tok::DynTz(i as u16));
            }
            return default_abbreviation(t)
                .map(|(off, dst)| if dst { Tok::Dtz(off) } else { Tok::Tz(off) });
        }
    })
}

/// PostgreSQL's `Default` `timezone_abbreviations` set, as
/// `pg_timezone_abbrevs` lists it on 15 (offset east in seconds, and whether
/// it is a daylight-saving abbreviation). The zone-dependent ones (`MSK`)
/// carry their current offset.
fn default_abbreviation(t: &str) -> Option<(i32, bool)> {
    Some(match t {
        "acdt" => (37800, true),
        "acsst" => (37800, true),
        "acst" => (34200, false),
        "act" => (-18000, false),
        "acwst" => (31500, false),
        "adt" => (-10800, true),
        "aedt" => (39600, true),
        "aesst" => (39600, true),
        "aest" => (36000, false),
        "aft" => (16200, false),
        "akdt" => (-28800, true),
        "akst" => (-32400, false),
        "almst" => (25200, true),
        "almt" => (21600, false),
        "amst" => (14400, false),
        "amt" => (-14400, false),
        "anast" => (43200, false),
        "anat" => (43200, false),
        "arst" => (-10800, false),
        "art" => (-10800, false),
        "ast" => (-14400, false),
        "awsst" => (32400, true),
        "awst" => (28800, false),
        "azost" => (0, true),
        "azot" => (-3600, false),
        "azst" => (14400, false),
        "azt" => (14400, false),
        "bdst" => (7200, true),
        "bdt" => (21600, false),
        "bnt" => (28800, false),
        "bort" => (28800, false),
        "bot" => (-14400, false),
        "bra" => (-10800, false),
        "brst" => (-7200, true),
        "brt" => (-10800, false),
        "bst" => (3600, true),
        "btt" => (21600, false),
        "cadt" => (37800, true),
        "cast" => (34200, false),
        "cct" => (28800, false),
        "cdt" => (-18000, true),
        "cest" => (7200, true),
        "cet" => (3600, false),
        "cetdst" => (7200, true),
        "chadt" => (49500, true),
        "chast" => (45900, false),
        "chut" => (36000, false),
        "ckt" => (-36000, false),
        "clst" => (-10800, true),
        "clt" => (-10800, true),
        "cot" => (-18000, false),
        "cst" => (-21600, false),
        "cxt" => (25200, false),
        "davt" => (25200, false),
        "ddut" => (36000, false),
        "easst" => (-18000, true),
        "east" => (-18000, true),
        "eat" => (10800, false),
        "edt" => (-14400, true),
        "eest" => (10800, true),
        "eet" => (7200, false),
        "eetdst" => (10800, true),
        "egst" => (0, true),
        "egt" => (-3600, false),
        "est" => (-18000, false),
        "fet" => (10800, false),
        "fjst" => (46800, true),
        "fjt" => (43200, false),
        "fkst" => (-10800, false),
        "fkt" => (-10800, false),
        "fnst" => (-3600, true),
        "fnt" => (-7200, false),
        "galt" => (-21600, false),
        "gamt" => (-32400, false),
        "gest" => (14400, false),
        "get" => (14400, false),
        "gft" => (-10800, false),
        "gilt" => (43200, false),
        "gmt" => (0, false),
        "gyt" => (-14400, false),
        "hkt" => (28800, false),
        "hst" => (-36000, false),
        "ict" => (25200, false),
        "idt" => (10800, true),
        "iot" => (21600, false),
        "irkst" => (28800, false),
        "irkt" => (28800, false),
        "irt" => (12600, false),
        "ist" => (7200, false),
        "jayt" => (32400, false),
        "jst" => (32400, false),
        "kdt" => (36000, true),
        "kgst" => (21600, true),
        "kgt" => (21600, false),
        "kost" => (39600, false),
        "krast" => (25200, false),
        "krat" => (25200, false),
        "kst" => (32400, false),
        "lhdt" => (37800, false),
        "lhst" => (37800, false),
        "ligt" => (36000, false),
        "lint" => (50400, false),
        "lkt" => (19800, false),
        "magst" => (39600, false),
        "magt" => (39600, false),
        "mart" => (-34200, false),
        "mawt" => (18000, false),
        "mdt" => (-21600, true),
        "mest" => (7200, true),
        "mesz" => (7200, true),
        "met" => (3600, false),
        "metdst" => (7200, true),
        "mez" => (3600, false),
        "mht" => (43200, false),
        "mmt" => (23400, false),
        "mpt" => (36000, false),
        "msd" => (14400, true),
        "msk" => (10800, false),
        "mst" => (-25200, false),
        "must" => (18000, true),
        "mut" => (14400, false),
        "mvt" => (18000, false),
        "myt" => (28800, false),
        "ndt" => (-9000, true),
        "nft" => (-12600, false),
        "novst" => (25200, false),
        "novt" => (25200, false),
        "npt" => (20700, false),
        "nst" => (-12600, false),
        "nut" => (-39600, false),
        "nzdt" => (46800, true),
        "nzst" => (43200, false),
        "nzt" => (43200, false),
        "omsst" => (21600, false),
        "omst" => (21600, false),
        "pdt" => (-25200, true),
        "pet" => (-18000, false),
        "petst" => (43200, false),
        "pett" => (43200, false),
        "pgt" => (36000, false),
        "pht" => (28800, false),
        "pkst" => (21600, true),
        "pkt" => (18000, false),
        "pmdt" => (-7200, true),
        "pmst" => (-10800, false),
        "pont" => (39600, false),
        "pst" => (-28800, false),
        "pwt" => (32400, false),
        "pyst" => (-10800, true),
        "pyt" => (-10800, false),
        "ret" => (14400, false),
        "sadt" => (37800, true),
        "sast" => (7200, false),
        "sct" => (14400, false),
        "sgt" => (28800, false),
        "taht" => (-36000, false),
        "tft" => (18000, false),
        "tjt" => (18000, false),
        "tkt" => (46800, false),
        "tmt" => (18000, false),
        "tot" => (46800, false),
        "trut" => (36000, false),
        "tvt" => (43200, false),
        "uct" => (0, false),
        "ulast" => (32400, true),
        "ulat" => (28800, false),
        "ut" => (0, false),
        "utc" => (0, false),
        "uyst" => (-7200, true),
        "uyt" => (-10800, false),
        "uzst" => (21600, true),
        "uzt" => (18000, false),
        "vet" => (-14400, false),
        "vlast" => (36000, false),
        "vlat" => (36000, false),
        "volt" => (10800, false),
        "vut" => (39600, false),
        "wadt" => (28800, true),
        "wakt" => (43200, false),
        "wast" => (25200, false),
        "wat" => (3600, false),
        "wdt" => (32400, true),
        "wet" => (0, false),
        "wetdst" => (3600, true),
        "wft" => (43200, false),
        "wgst" => (-7200, true),
        "wgt" => (-10800, false),
        "xjt" => (21600, false),
        "yakst" => (32400, false),
        "yakt" => (32400, false),
        "yapt" => (36000, false),
        "yekst" => (21600, true),
        "yekt" => (18000, false),
        "z" => (0, false),
        "zulu" => (0, false),
        _ => return None,
    })
}

#[derive(Default)]
struct Tm {
    year: i32,
    mon: i32,
    mday: i32,
    hour: i32,
    min: i32,
    sec: i32,
    yday: i32,
    fsec: i64,
}

fn strtoint(s: &str) -> (i64, &str) {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let v = s[..digits].parse::<i64>().unwrap_or(i64::MAX);
    (v, &s[digits..])
}

fn fraction(s: &str) -> std::result::Result<i64, Fail> {
    // s starts with '.'
    let f: f64 = format!("0{s}").parse().map_err(|_| Fail::BadFormat)?;
    Ok((f * 1_000_000.0).round() as i64)
}

fn decode_time(s: &str, tm: &mut Tm) -> std::result::Result<u32, Fail> {
    let (h, rest) = strtoint(s);
    let rest = rest.strip_prefix(':').ok_or(Fail::BadFormat)?;
    let (m, rest) = strtoint(rest);
    tm.hour = h as i32;
    if rest.is_empty() {
        tm.min = m as i32;
        tm.sec = 0;
        tm.fsec = 0;
    } else if rest.starts_with('.') {
        // mm:ss.sss
        tm.fsec = fraction(rest)?;
        tm.sec = m as i32;
        tm.min = h as i32;
        tm.hour = 0;
    } else if let Some(r) = rest.strip_prefix(':') {
        tm.min = m as i32;
        let (sec, r) = strtoint(r);
        tm.sec = sec as i32;
        if r.is_empty() {
            tm.fsec = 0;
        } else if r.starts_with('.') {
            tm.fsec = fraction(r)?;
        } else {
            return Err(Fail::BadFormat);
        }
    } else {
        return Err(Fail::BadFormat);
    }
    if tm.hour < 0 || tm.min < 0 || tm.min > 59 || tm.sec < 0 || tm.sec > 60 || tm.fsec > 1_000_000
    {
        return Err(Fail::FieldOverflow);
    }
    Ok(TIME_M)
}

fn decode_timezone(s: &str) -> std::result::Result<i32, Fail> {
    let neg = s.starts_with('-');
    let body = s.get(1..).ok_or(Fail::BadFormat)?;
    let (hr, mut rest) = strtoint(body);
    let (mut min, mut sec) = (0i64, 0i64);
    let mut hr = hr;
    if let Some(r) = rest.strip_prefix(':') {
        let (m, r) = strtoint(r);
        min = m;
        rest = r;
        if let Some(r) = rest.strip_prefix(':') {
            let (sv, r) = strtoint(r);
            sec = sv;
            rest = r;
        }
    } else if rest.is_empty() && body.len() > 2 {
        min = hr % 100;
        hr /= 100;
    }
    if !(0..=15).contains(&hr) || !(0..60).contains(&min) || !(0..60).contains(&sec) {
        return Err(Fail::TzOverflow);
    }
    if !rest.is_empty() {
        return Err(Fail::BadFormat);
    }
    let tz = ((hr * 60 + min) * 60 + sec) as i32;
    Ok(if neg { -tz } else { tz })
}

fn decode_number_field(
    s: &str,
    fmask: u32,
    tm: &mut Tm,
    is2digits: &mut bool,
) -> std::result::Result<u32, Fail> {
    let mut str_ = s.to_string();
    if let Some(dot) = s.find('.') {
        let frac: f64 = format!("0{}", &s[dot..])
            .parse()
            .map_err(|_| Fail::BadFormat)?;
        tm.fsec = (frac * 1_000_000.0).round() as i64;
        str_.truncate(dot);
    } else if fmask & DATE_M != DATE_M && str_.len() >= 6 {
        let len = str_.len();
        tm.mday = str_[len - 2..].parse().map_err(|_| Fail::BadFormat)?;
        tm.mon = str_[len - 4..len - 2]
            .parse()
            .map_err(|_| Fail::BadFormat)?;
        tm.year = str_[..len - 4].parse().map_err(|_| Fail::BadFormat)?;
        if len - 4 == 2 {
            *is2digits = true;
        }
        return Ok(DATE_M);
    }
    let len = str_.len();
    if fmask & TIME_M != TIME_M {
        if len == 6 {
            tm.sec = str_[4..6].parse().map_err(|_| Fail::BadFormat)?;
            tm.min = str_[2..4].parse().map_err(|_| Fail::BadFormat)?;
            tm.hour = str_[..2].parse().map_err(|_| Fail::BadFormat)?;
            return Ok(TIME_M);
        } else if len == 4 {
            tm.sec = 0;
            tm.min = str_[2..4].parse().map_err(|_| Fail::BadFormat)?;
            tm.hour = str_[..2].parse().map_err(|_| Fail::BadFormat)?;
            return Ok(TIME_M);
        }
    }
    Err(Fail::BadFormat)
}

fn decode_number(
    s: &str,
    have_text_month: bool,
    fmask: u32,
    tm: &mut Tm,
    is2digits: &mut bool,
) -> std::result::Result<u32, Fail> {
    let flen = s.len();
    let (val, rest) = strtoint(s);
    if rest.len() == s.len() {
        return Err(Fail::BadFormat);
    }
    if val > i64::from(i32::MAX) {
        return Err(Fail::FieldOverflow);
    }
    let val = val as i32;
    if rest.starts_with('.') {
        if s.len() - rest.len() > 2 {
            return decode_number_field(s, fmask | DATE_M, tm, is2digits);
        }
        tm.fsec = fraction(rest)?;
    } else if !rest.is_empty() {
        return Err(Fail::BadFormat);
    }
    if flen == 3 && fmask & DATE_M == YEAR && (1..=366).contains(&val) {
        tm.yday = val;
        return Ok(DOY | MONTH | DAY);
    }
    let order = date_order();
    let tmask = match fmask & DATE_M {
        0 => {
            if flen >= 3 || order == DateOrder::Ymd {
                tm.year = val;
                YEAR
            } else if order == DateOrder::Dmy {
                tm.mday = val;
                DAY
            } else {
                tm.mon = val;
                MONTH
            }
        }
        x if x == YEAR => {
            tm.mon = val;
            MONTH
        }
        x if x == MONTH => {
            if have_text_month {
                if flen >= 3 || order == DateOrder::Ymd {
                    tm.year = val;
                    YEAR
                } else {
                    tm.mday = val;
                    DAY
                }
            } else {
                tm.mday = val;
                DAY
            }
        }
        x if x == YEAR | MONTH => {
            if have_text_month && flen >= 3 && *is2digits {
                tm.mday = tm.year;
                tm.year = val;
                *is2digits = false;
                DAY
            } else {
                tm.mday = val;
                DAY
            }
        }
        x if x == DAY => {
            tm.mon = val;
            MONTH
        }
        x if x == MONTH | DAY => {
            tm.year = val;
            YEAR
        }
        x if x == DATE_M => return decode_number_field(s, fmask, tm, is2digits),
        _ => return Err(Fail::BadFormat),
    };
    if tmask == YEAR {
        *is2digits = flen <= 2;
    }
    Ok(tmask)
}

fn decode_date(
    s: &str,
    fmask_in: u32,
    tm: &mut Tm,
    is2digits: &mut bool,
) -> std::result::Result<u32, Fail> {
    let mut fields: Vec<String> = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        while i < cs.len() && !cs[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i >= cs.len() {
            return Err(Fail::BadFormat);
        }
        let start = i;
        if cs[i].is_ascii_digit() {
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            while i < cs.len() && cs[i].is_ascii_alphabetic() {
                i += 1;
            }
        }
        fields.push(cs[start..i].iter().collect());
        if i < cs.len() {
            i += 1;
        }
    }
    let mut fmask = fmask_in;
    let mut tmask = 0;
    let mut have_text_month = false;
    let mut used = vec![false; fields.len()];
    for (k, f) in fields.iter().enumerate() {
        if f.starts_with(|c: char| c.is_ascii_alphabetic()) {
            match keyword(f) {
                Some(Tok::Ignore) => {
                    used[k] = true;
                    continue;
                }
                Some(Tok::Month(m)) => {
                    if fmask & MONTH != 0 {
                        return Err(Fail::BadFormat);
                    }
                    tm.mon = m as i32;
                    have_text_month = true;
                    fmask |= MONTH;
                    tmask |= MONTH;
                    used[k] = true;
                }
                _ => return Err(Fail::BadFormat),
            }
        }
    }
    for (k, f) in fields.iter().enumerate() {
        if used[k] {
            continue;
        }
        if f.is_empty() {
            return Err(Fail::BadFormat);
        }
        let dmask = decode_number(f, have_text_month, fmask, tm, is2digits)?;
        if fmask & dmask != 0 {
            return Err(Fail::BadFormat);
        }
        fmask |= dmask;
        tmask |= dmask;
    }
    if (fmask & !(DOY | TZ)) != DATE_M {
        return Err(Fail::BadFormat);
    }
    Ok(tmask)
}

const DAYS: [[i32; 12]; 2] = [
    [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
];

fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn date2j(y: i32, m: i32, d: i32) -> i64 {
    let (mut y, mut m) = (i64::from(y), i64::from(m));
    if m > 2 {
        m += 1;
        y += 4800;
    } else {
        m += 13;
        y += 4799;
    }
    let century = y / 100;
    let mut julian = y * 365 - 32167;
    julian += y / 4 - century + century / 4;
    julian += 7834 * m / 256 + i64::from(d);
    julian
}

fn j2date(jd: i64) -> (i32, i32, i32) {
    let mut julian = jd as u64 as i64 + 32044;
    let mut quad = julian / 146097;
    let extra = (julian - quad * 146097) * 4 + 3;
    julian += 60 + quad * 3 + extra / 146097;
    quad = julian / 1461;
    julian -= quad * 1461;
    let mut y = julian * 4 / 1461;
    julian = if y != 0 {
        (julian + 305) % 365
    } else {
        (julian + 306) % 366
    } + 123;
    y += quad * 4;
    let year = y - 4800;
    quad = julian * 2141 / 65536;
    let day = julian - 7834 * quad / 256;
    let month = (quad + 10) % 12 + 1;
    (year as i32, month as i32, day as i32)
}

fn validate_date(
    fmask: u32,
    isjulian: bool,
    is2digits: bool,
    bc: bool,
    tm: &mut Tm,
) -> std::result::Result<(), Fail> {
    if fmask & YEAR != 0 {
        if isjulian {
        } else if bc {
            if tm.year <= 0 {
                return Err(Fail::FieldOverflow);
            }
            tm.year = -(tm.year - 1);
        } else if is2digits {
            if tm.year < 70 {
                tm.year += 2000;
            } else if tm.year < 100 {
                tm.year += 1900;
            }
        } else if tm.year <= 0 {
            return Err(Fail::FieldOverflow);
        }
    }
    if fmask & DOY != 0 {
        let (y, m, d) = j2date(date2j(tm.year, 1, 1) + i64::from(tm.yday) - 1);
        tm.year = y;
        tm.mon = m;
        tm.mday = d;
    }
    if fmask & MONTH != 0 && !(1..=12).contains(&tm.mon) {
        return Err(Fail::MdFieldOverflow);
    }
    if fmask & DAY != 0 && !(1..=31).contains(&tm.mday) {
        return Err(Fail::MdFieldOverflow);
    }
    if fmask & DATE_M == DATE_M
        && tm.mday > DAYS[usize::from(is_leap(tm.year))][(tm.mon - 1) as usize]
    {
        return Err(Fail::FieldOverflow);
    }
    Ok(())
}

/// Decode `text` as `DecodeDateTime` does. `type_name` names the target in
/// the error (`date`, `timestamp`, `timestamp with time zone`).
pub fn parse(text: &str, type_name: &str) -> Result<Parsed> {
    decode(text, false).map_err(|f| to_error(f, text, type_name))
}

/// A `time` / `timetz` input: `DecodeTimeOnly`. A date part is accepted and
/// ignored; a bare six-digit number is `HHMMSS` rather than `YYMMDD`; the
/// result is the time of day (`has_date` false), with `24:00:00` allowed and a
/// leap second carried into the next minute.
pub fn parse_time_only(text: &str, type_name: &str) -> Result<Parsed> {
    decode(text, true).map_err(|f| to_error(f, text, type_name))
}

fn decode(text: &str, time_only: bool) -> std::result::Result<Parsed, Fail> {
    let fields = parse_fields(text)?;
    let nf = fields.len();
    let mut tm = Tm::default();
    let mut fmask: u32 = 0;
    let mut ptype: u32 = 0;
    let mut mer: Option<bool> = None; // Some(true) = PM
    let mut have_text_month = false;
    let mut isjulian = false;
    let mut is2digits = false;
    let mut bc = false;
    let mut out = Parsed::default();
    let mut tz: Option<i32> = None;
    let mut dyn_zone: Option<u16> = None;
    for i in 0..nf {
        let (f, kind) = (&fields[i].0, fields[i].1);
        let tmask: u32;
        match kind {
            Ftype::Date => {
                if ptype == U_JULIAN {
                    let (val, rest) = strtoint(f);
                    let (y, m, d) = j2date(val);
                    tm.year = y;
                    tm.mon = m;
                    tm.mday = d;
                    isjulian = true;
                    tz = Some(decode_timezone(rest)?);
                    tmask = DATE_M | TIME_M | TZ;
                    ptype = 0;
                } else if ptype != 0 || fmask & (MONTH | DAY) == (MONTH | DAY) {
                    if f.starts_with(|c: char| c.is_ascii_digit()) || ptype != 0 {
                        if ptype != 0 {
                            if ptype != U_TIME {
                                return Err(Fail::BadFormat);
                            }
                            ptype = 0;
                        }
                        if fmask & TIME_M == TIME_M {
                            return Err(Fail::BadFormat);
                        }
                        let dash = f.find('-').ok_or(Fail::BadFormat)?;
                        tz = Some(decode_timezone(&f[dash..])?);
                        let m = decode_number_field(&f[..dash], fmask, &mut tm, &mut is2digits)?;
                        tmask = m | TZ;
                    } else {
                        out.zone = Some(f.clone());
                        tmask = TZ;
                    }
                } else {
                    tmask = decode_date(f, fmask, &mut tm, &mut is2digits)?;
                }
            }
            Ftype::Time => {
                if ptype != 0 {
                    if ptype != U_TIME {
                        return Err(Fail::BadFormat);
                    }
                    ptype = 0;
                }
                tmask = decode_time(f, &mut tm)?;
                let total = i64::from(tm.hour) * 3600 + i64::from(tm.min) * 60 + i64::from(tm.sec);
                if total > 86_400 || (total == 86_400 && tm.fsec > 0) {
                    return Err(Fail::FieldOverflow);
                }
            }
            Ftype::Tz => {
                tz = Some(decode_timezone(f)?);
                tmask = TZ;
            }
            Ftype::Number => {
                if ptype != 0 {
                    let (val, rest) = strtoint(f);
                    if rest.starts_with('.') {
                        if !matches!(ptype, U_JULIAN | U_TIME | U_SECOND) {
                            return Err(Fail::BadFormat);
                        }
                    } else if !rest.is_empty() {
                        return Err(Fail::BadFormat);
                    }
                    let val = val as i32;
                    tmask = match ptype {
                        U_YEAR => {
                            tm.year = val;
                            YEAR
                        }
                        U_MONTH => {
                            if fmask & MONTH != 0 && fmask & HOUR != 0 {
                                tm.min = val;
                                MINUTE
                            } else {
                                tm.mon = val;
                                MONTH
                            }
                        }
                        U_DAY => {
                            tm.mday = val;
                            DAY
                        }
                        U_HOUR => {
                            tm.hour = val;
                            HOUR
                        }
                        U_MINUTE => {
                            tm.min = val;
                            MINUTE
                        }
                        U_SECOND => {
                            tm.sec = val;
                            if rest.starts_with('.') {
                                tm.fsec = fraction(rest)?;
                                ALL_SECS_M
                            } else {
                                SECOND
                            }
                        }
                        U_JULIAN => {
                            if val < 0 {
                                return Err(Fail::FieldOverflow);
                            }
                            let (y, m, d) = j2date(i64::from(val));
                            tm.year = y;
                            tm.mon = m;
                            tm.mday = d;
                            isjulian = true;
                            let mut m_ = DATE_M;
                            if rest.starts_with('.') {
                                let frac: f64 =
                                    format!("0{rest}").parse().map_err(|_| Fail::BadFormat)?;
                                let us = (frac * 86_400_000_000.0).round() as i64;
                                tm.hour = (us / 3_600_000_000) as i32;
                                tm.min = ((us / 60_000_000) % 60) as i32;
                                tm.sec = ((us / 1_000_000) % 60) as i32;
                                tm.fsec = us % 1_000_000;
                                m_ |= TIME_M;
                            }
                            m_
                        }
                        U_TIME => {
                            let m =
                                decode_number_field(f, fmask | DATE_M, &mut tm, &mut is2digits)?;
                            if m != TIME_M {
                                return Err(Fail::BadFormat);
                            }
                            m
                        }
                        _ => return Err(Fail::BadFormat),
                    };
                    ptype = 0;
                } else {
                    let flen = f.len();
                    let dot = f.find('.');
                    if time_only {
                        let int_len = dot.unwrap_or(flen);
                        tmask = if int_len > 2 || (dot.is_none() && flen > 4) {
                            decode_number_field(f, fmask | DATE_M, &mut tm, &mut is2digits)?
                        } else {
                            decode_number(f, false, fmask | DATE_M, &mut tm, &mut is2digits)?
                        };
                    } else if dot.is_some() && fmask & DATE_M == 0 {
                        tmask = decode_date(f, fmask, &mut tm, &mut is2digits)?;
                    } else if let Some(d) = dot.filter(|d| *d > 2) {
                        let _ = d;
                        tmask = decode_number_field(f, fmask, &mut tm, &mut is2digits)?;
                    } else if flen >= 6 && (fmask & DATE_M == 0 || fmask & TIME_M == 0) {
                        tmask = decode_number_field(f, fmask, &mut tm, &mut is2digits)?;
                    } else {
                        tmask = decode_number(f, have_text_month, fmask, &mut tm, &mut is2digits)?;
                    }
                }
            }
            Ftype::String | Ftype::Special => match keyword(f) {
                Some(Tok::Ignore) | Some(Tok::Dow) => {
                    if keyword(f) == Some(Tok::Ignore) {
                        continue;
                    }
                    tmask = 1 << 16; // DOW
                }
                Some(Tok::Reserved(r)) => {
                    tmask = DATE_M | TIME_M | TZ;
                    match r {
                        Res::Epoch => out.special = Some(Special::Epoch),
                        Res::Late => out.special = Some(Special::Infinity),
                        Res::Early => out.special = Some(Special::NegInfinity),
                        Res::Now => out.special = Some(Special::Now),
                        Res::Today | Res::Tomorrow | Res::Yesterday => {
                            out.relative_day = Some(match r {
                                Res::Yesterday => -1,
                                Res::Tomorrow => 1,
                                _ => 0,
                            });
                            let t = DATE_M;
                            if t & fmask != 0 {
                                return Err(Fail::BadFormat);
                            }
                            fmask |= t;
                            continue;
                        }
                        Res::Zulu => {
                            tm.hour = 0;
                            tm.min = 0;
                            tm.sec = 0;
                            tz = Some(0);
                            let t = TIME_M | TZ;
                            if t & fmask != 0 {
                                return Err(Fail::BadFormat);
                            }
                            fmask |= t;
                            continue;
                        }
                    }
                }
                Some(Tok::Month(m)) => {
                    let mut t = MONTH;
                    if fmask & MONTH != 0
                        && !have_text_month
                        && fmask & DAY == 0
                        && (1..=31).contains(&tm.mon)
                    {
                        tm.mday = tm.mon;
                        t = DAY;
                    }
                    have_text_month = true;
                    tm.mon = m as i32;
                    tmask = t;
                }
                Some(Tok::DtzMod(v)) => {
                    tmask = DTZMOD;
                    tz = Some(tz.unwrap_or(0) + v);
                }
                Some(Tok::Dtz(v)) | Some(Tok::Tz(v)) => {
                    tmask = TZ;
                    tz = Some(v);
                }
                Some(Tok::DynTz(z)) => {
                    tmask = TZ;
                    // The current offset stands until the date is known.
                    tz =
                        Some(default_abbreviation(DYNAMIC[usize::from(z)].0).map_or(0, |(o, _)| o));
                    dyn_zone = Some(z);
                }
                Some(Tok::AmPm(pm)) => {
                    tmask = 1 << 17; // AMPM
                    mer = Some(pm);
                }
                Some(Tok::AdBc(is_bc)) => {
                    tmask = 1 << 18; // ADBC
                    bc = is_bc;
                }
                Some(Tok::Units(u)) => {
                    ptype = u;
                    continue;
                }
                Some(Tok::IsoTime) => {
                    if !time_only && fmask & DATE_M != DATE_M {
                        return Err(Fail::BadFormat);
                    }
                    if i + 1 >= nf
                        || !matches!(fields[i + 1].1, Ftype::Number | Ftype::Time | Ftype::Date)
                    {
                        return Err(Fail::BadFormat);
                    }
                    ptype = U_TIME;
                    continue;
                }
                None => {
                    // An all-alpha time zone name (`UTC` is handled above).
                    if chrono_tz_known(f) {
                        out.zone = Some(f.clone());
                        tmask = TZ;
                    } else {
                        return Err(Fail::BadFormat);
                    }
                }
            },
        }
        if tmask & fmask != 0 {
            return Err(Fail::BadFormat);
        }
        fmask |= tmask;
    }
    if let Some(s) = out.special {
        if nf != 1 {
            return Err(Fail::BadFormat);
        }
        let _ = s;
        return Ok(out);
    }
    if time_only {
        return finish_time_only(fmask, mer, tm, tz, out);
    }
    if out.relative_day.is_none() {
        validate_date(fmask, isjulian, is2digits, bc, &mut tm)?;
    }
    if let Some(pm) = mer {
        if tm.hour > 12 {
            return Err(Fail::FieldOverflow);
        }
        if !pm && tm.hour == 12 {
            tm.hour = 0;
        } else if pm && tm.hour != 12 {
            tm.hour += 12;
        }
    }
    if out.relative_day.is_none() && fmask & DATE_M != DATE_M {
        return Err(Fail::BadFormat);
    }
    out.has_date = true;
    out.year = tm.year;
    out.month = tm.mon as u32;
    out.day = tm.mday as u32;
    out.hour = tm.hour as u32;
    out.minute = tm.min as u32;
    out.second = tm.sec as u32;
    out.micros = tm.fsec;
    out.offset = tz;
    if let Some(z) = dyn_zone {
        if let Some(o) = dynamic_offset(
            z,
            tm.year,
            tm.mon as u32,
            tm.mday as u32,
            tm.hour as u32,
            tm.min as u32,
            tm.sec as u32,
        ) {
            out.offset = Some(o);
        }
    }
    Ok(out)
}

/// The tail of `DecodeTimeOnly`: the meridian, the range checks, and the
/// normalisation of a 60th second.
fn finish_time_only(
    fmask: u32,
    mer: Option<bool>,
    mut tm: Tm,
    tz: Option<i32>,
    mut out: Parsed,
) -> std::result::Result<Parsed, Fail> {
    if fmask & TIME_M == 0 && tz.is_none() {
        return Err(Fail::BadFormat);
    }
    if let Some(pm) = mer {
        if tm.hour > 12 {
            return Err(Fail::FieldOverflow);
        }
        if !pm && tm.hour == 12 {
            tm.hour = 0;
        } else if pm && tm.hour != 12 {
            tm.hour += 12;
        }
    }
    if tm.hour < 0
        || tm.min < 0
        || tm.min > 59
        || tm.sec < 0
        || tm.sec > 60
        || tm.hour > 24
        || (tm.hour == 24 && (tm.min > 0 || tm.sec > 0 || tm.fsec > 0))
        || tm.fsec < 0
        || tm.fsec > 1_000_000
    {
        return Err(Fail::FieldOverflow);
    }
    let mut total = (i64::from(tm.hour) * 3600 + i64::from(tm.min) * 60 + i64::from(tm.sec))
        * 1_000_000
        + tm.fsec;
    if total > 86_400_000_000 {
        return Err(Fail::FieldOverflow);
    }
    out.has_date = false;
    out.hour = (total / 3_600_000_000) as u32;
    total %= 3_600_000_000;
    out.minute = (total / 60_000_000) as u32;
    total %= 60_000_000;
    out.second = (total / 1_000_000) as u32;
    out.micros = total % 1_000_000;
    out.offset = tz;
    Ok(out)
}

/// A time zone ABBREVIATION's offset in seconds east (`est` is -18000).
pub fn abbreviation_offset(name: &str) -> Option<i32> {
    match keyword(&name.to_ascii_lowercase()) {
        Some(Tok::Tz(v)) | Some(Tok::Dtz(v)) => Some(v),
        Some(Tok::DynTz(z)) => default_abbreviation(DYNAMIC[usize::from(z)].0).map(|(o, _)| o),
        _ => None,
    }
}

fn chrono_tz_known(name: &str) -> bool {
    name.parse::<chrono_tz::Tz>().is_ok()
        || chrono_tz::TZ_VARIANTS
            .iter()
            .any(|z| z.name().eq_ignore_ascii_case(name))
}

/// A zone name, case-insensitively, as chrono knows it.
pub fn resolve_zone(name: &str) -> Option<chrono_tz::Tz> {
    name.parse::<chrono_tz::Tz>().ok().or_else(|| {
        chrono_tz::TZ_VARIANTS
            .iter()
            .find(|z| z.name().eq_ignore_ascii_case(name))
            .copied()
    })
}
