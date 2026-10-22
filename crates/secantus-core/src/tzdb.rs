//! mongod's own time-zone database, read the way mongod reads it.
//!
//! mongod does not consult the system's zoneinfo: it compiles in timelib's
//! `timezonedb.h` -- for 8.2.11, timezonedb "2024.2", which is IANA tzdata
//! 2024b built with `backzone` -- and answers every time-zone question from
//! it. `tzdata/timezonedb.bin` and `timezonedb.idx` are that data, copied byte
//! for byte by `tools/timezonedb_extract.py` (RM-249; Joe chose mongod's data
//! over a tz crate on 2026-10-22). This module ports the parts of timelib that
//! read it: `timelib_parse_tzfile`, `timelib_fetch_timezone_offset` and the
//! POSIX-string rules that carry a zone past its last stored transition.
//!
//! So a zone's history is mongod's (Amsterdam in 1900 is +0019, from
//! `backzone`), its future is mongod's (New York keeps daylight saving in
//! 2100 and 9000, where chrono-tz's tables stop at 2099), and its names are
//! mongod's, matched exactly as mongod's map matches them (`Factory`, but
//! not `factory`).

use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../tzdata/timezonedb.bin");
static INDEX: &str = include_str!("../tzdata/timezonedb.idx");

/// One time type: a UTC offset in seconds and whether it is daylight time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TimeType {
    pub offset: i64,
    pub is_dst: bool,
}

/// A zone as `timelib_tzinfo` holds it, minus what mongod never reads.
pub(crate) struct Zone {
    transitions: Vec<i64>,
    transition_types: Vec<u8>,
    types: Vec<TimeType>,
    abbrs: Vec<(usize, String)>,
    posix: Option<Posix>,
}

/// The zone named `name`, exactly as mongod's `TimeZoneDatabase` holds it.
pub(crate) fn zone(name: &str) -> Option<&'static Zone> {
    let (names, zones) = database();
    let (pos, parsed) = &zones[*names.get(name)?];
    parsed.get_or_init(|| parse(*pos)).as_ref()
}

/// Whether `name` is a zone mongod knows.
pub(crate) fn is_zone(name: &str) -> bool {
    database().0.contains_key(name)
}

/// The names, and each zone's offset in the data with its parse, made on
/// first use.
type Database = (
    HashMap<&'static str, usize>,
    Vec<(usize, OnceLock<Option<Zone>>)>,
);

fn database() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(|| {
        let names = entries()
            .enumerate()
            .map(|(i, (name, _))| (name, i))
            .collect();
        let zones = entries().map(|(_, pos)| (pos, OnceLock::new())).collect();
        (names, zones)
    })
}

fn entries() -> impl Iterator<Item = (&'static str, usize)> {
    INDEX.lines().skip(1).map(|line| {
        let (name, pos) = line
            .split_once('\t')
            .expect("timezonedb.idx: name<TAB>offset");
        (name, pos.parse().expect("timezonedb.idx: a decimal offset"))
    })
}

/// A big-endian reader over the data, as timelib's `*tzf += n` walks it.
struct Reader {
    at: usize,
}

impl Reader {
    fn take(&mut self, n: usize) -> &'static [u8] {
        let s = &DATA[self.at..self.at + n];
        self.at += n;
        s
    }
    fn u32(&mut self) -> usize {
        u32::from_be_bytes(self.take(4).try_into().unwrap()) as usize
    }
}

/// `timelib_parse_tzfile` from `seek_to_tz_position` on. `None` where timelib
/// returns NULL -- which mongod turns into a fatal assertion at startup, so it
/// does not happen with this data.
fn parse(pos: usize) -> Option<Zone> {
    let mut r = Reader { at: pos };
    // `read_php_preamble`: "PHP" + version, the BC flag, a country code and
    // thirteen reserved bytes.
    if r.take(3) != b"PHP" {
        return None;
    }
    r.take(17);
    // The 32-bit header and data, which timelib skips.
    let counts32: Vec<usize> = (0..6).map(|_| r.u32()).collect();
    let (isgmt, isstd, leap, time, typ, chars) = (
        counts32[0],
        counts32[1],
        counts32[2],
        counts32[3],
        counts32[4],
        counts32[5],
    );
    r.take(time * 5 + typ * 6 + chars + leap * 8 + isstd + isgmt);
    // `skip_64bit_preamble`: "TZif2" / "3" / "4" and fifteen bytes.
    if !matches!(r.take(5), b"TZif2" | b"TZif3" | b"TZif4") {
        return None;
    }
    r.take(15);
    let counts: Vec<usize> = (0..6).map(|_| r.u32()).collect();
    let (isgmt, isstd, leap, time, typ, chars) = (
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5],
    );
    let transitions: Vec<i64> = r
        .take(time * 8)
        .chunks(8)
        .map(|b| i64::from_be_bytes(b.try_into().unwrap()))
        .collect();
    if transitions.windows(2).any(|w| w[1] <= w[0]) {
        return None; // TIMELIB_ERROR_CORRUPT_TRANSITIONS_DONT_INCREASE
    }
    let transition_types = r.take(time).to_vec();
    let raw_types = r.take(typ * 6);
    let abbr_chars = r.take(chars);
    let abbr_at = |idx: usize| -> String {
        let s = &abbr_chars[idx.min(abbr_chars.len())..];
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    };
    let mut types = Vec::with_capacity(typ + 2);
    let mut abbrs = Vec::with_capacity(typ + 2);
    for t in raw_types.chunks(6) {
        types.push(TimeType {
            offset: i64::from(i32::from_be_bytes(t[..4].try_into().unwrap())),
            is_dst: t[4] != 0,
        });
        abbrs.push((t[5] as usize, abbr_at(t[5] as usize)));
    }
    r.take(leap * 12 + isstd + isgmt);
    // `read_posix_string`: delimited by newlines.
    r.take(1);
    let start = r.at;
    while DATA[r.at] != b'\n' {
        r.at += 1;
    }
    let posix_string = std::str::from_utf8(&DATA[start..r.at]).ok()?;
    let mut zone = Zone {
        transitions,
        transition_types,
        types,
        abbrs,
        posix: None,
    };
    if !posix_string.is_empty() {
        zone.posix = Some(integrate_posix(&mut zone, parse_posix(posix_string)?));
    }
    Some(zone)
}

/// `find_ttinfo_index`: the first type with this offset, flag and
/// abbreviation.
fn find_type(zone: &Zone, offset: i64, is_dst: bool, abbr: &str) -> Option<usize> {
    (0..zone.types.len()).find(|&i| {
        zone.types[i].offset == offset && zone.types[i].is_dst == is_dst && zone.abbrs[i].1 == abbr
    })
}

/// `integrate_posix_string`, faithfully: when the standard type is NEW it is
/// added and the function returns before looking at the daylight type, whose
/// index then stays 0 (`calloc`'d) -- timelib's behaviour, kept.
fn integrate_posix(zone: &mut Zone, mut p: Posix) -> Posix {
    let add = |zone: &mut Zone, offset: i64, is_dst: bool, abbr: &str| {
        zone.types.push(TimeType { offset, is_dst });
        zone.abbrs.push((usize::MAX, abbr.to_string()));
        zone.types.len() - 1
    };
    match find_type(zone, p.std_offset, false, &p.std) {
        Some(i) => p.std_type = i,
        None => {
            p.std_type = add(zone, p.std_offset, false, &p.std.clone());
            return p;
        }
    }
    if let Some(dst) = p.dst.as_mut() {
        dst.dst_type = match find_type(zone, dst.dst_offset, true, &dst.name) {
            Some(i) => i,
            None => add(zone, dst.dst_offset, true, &dst.name.clone()),
        };
    }
    p
}

impl Zone {
    /// `timelib_get_time_zone_offset_info`: the type in force at `ts` (UTC
    /// seconds) and the transition that started it (`i64::MIN` for none).
    /// `None` where timelib finds no type, which its callers read as offset
    /// 0, transition 0, standard time.
    pub(crate) fn offset_info(&self, ts: i64) -> Option<(TimeType, i64)> {
        let n = self.transitions.len();
        if n == 0 {
            if let Some(p) = &self.posix {
                return self.posix_offset(p, ts).map(|(t, _)| (t, i64::MIN));
            }
            return (self.types.len() == 1).then(|| (self.types[0], i64::MIN));
        }
        if ts < self.transitions[0] {
            return Some((self.types[0], i64::MIN));
        }
        if ts >= self.transitions[n - 1] {
            if let Some(p) = &self.posix {
                return self.posix_offset(p, ts);
            }
            return Some((
                self.types[self.transition_types[n - 1] as usize],
                self.transitions[n - 1],
            ));
        }
        let (mut left, mut right) = (0, n - 1);
        while right - left > 1 {
            let mid = (left + right) >> 1;
            if ts < self.transitions[mid] {
                right = mid;
            } else {
                left = mid;
            }
        }
        Some((
            self.types[self.transition_types[left] as usize],
            self.transitions[left],
        ))
    }

    /// `timelib_fetch_posix_timezone_offset`.
    fn posix_offset(&self, p: &Posix, ts: i64) -> Option<(TimeType, i64)> {
        let Some(dst) = &p.dst else {
            let since = self.transitions.last().copied().unwrap_or(i64::MIN);
            return Some((self.types[p.std_type], since));
        };
        let year = utc_year(ts);
        let mut times = Vec::with_capacity(6);
        for y in [year - 1, year, year + 1] {
            let start = ts_at_start_of_year(y);
            let begin = start + calc_transition(&dst.begin, y) + dst.begin.hour - p.std_offset;
            let end = start + calc_transition(&dst.end, y) + dst.end.hour - dst.dst_offset;
            if begin < end {
                times.push((begin, dst.dst_type));
                times.push((end, p.std_type));
            } else {
                times.push((end, p.std_type));
                times.push((begin, dst.dst_type));
            }
        }
        (1..times.len())
            .find(|&i| ts < times[i].0)
            .map(|i| (self.types[times[i - 1].1], times[i - 1].0))
    }
}

/// A POSIX TZ string as `timelib_parse_posix_str` reads it.
struct Posix {
    std: String,
    std_offset: i64,
    std_type: usize,
    dst: Option<PosixDst>,
}

struct PosixDst {
    name: String,
    dst_offset: i64,
    dst_type: usize,
    begin: TransitionSpec,
    end: TransitionSpec,
}

enum SpecKind {
    JulianNoFeb29(i64),
    JulianFeb29(i64),
    Mwd { month: i64, week: i64, dow: i64 },
}

struct TransitionSpec {
    kind: SpecKind,
    hour: i64,
}

const SECS_PER_DAY: i64 = 86_400;

fn parse_posix(s: &str) -> Option<Posix> {
    let b = s.as_bytes();
    let mut at = 0;
    let std = read_description(b, &mut at)?;
    let std_offset = read_offset(b, &mut at)?;
    if at == b.len() {
        return Some(Posix {
            std,
            std_offset,
            std_type: 0,
            dst: None,
        });
    }
    let mut dst_offset = std_offset + 3600;
    let name = read_description(b, &mut at)?;
    if at < b.len() && b[at] != b',' {
        dst_offset = read_offset(b, &mut at)?;
    }
    if b.get(at) != Some(&b',') {
        return None;
    }
    at += 1;
    let begin = read_transition_spec(b, &mut at)?;
    if b.get(at) != Some(&b',') {
        return None;
    }
    at += 1;
    let end = read_transition_spec(b, &mut at)?;
    if at != b.len() {
        return None;
    }
    Some(Posix {
        std,
        std_offset,
        std_type: 0,
        dst: Some(PosixDst {
            name,
            dst_offset,
            dst_type: 0,
            begin,
            end,
        }),
    })
}

/// `"<" .+? ">"` or `[A-Za-z]+`, never empty.
fn read_description(b: &[u8], at: &mut usize) -> Option<String> {
    let start = *at;
    if b.get(*at) == Some(&b'<') {
        *at += 1;
        while *at < b.len() && b[*at] != b'>' {
            *at += 1;
        }
        if *at == b.len() {
            return None;
        }
        *at += 1;
        let inner = &b[start + 1..*at - 1];
        return (!inner.is_empty()).then(|| String::from_utf8_lossy(inner).into_owned());
    }
    while *at < b.len() && b[*at].is_ascii_alphabetic() {
        *at += 1;
    }
    (*at > start).then(|| String::from_utf8_lossy(&b[start..*at]).into_owned())
}

/// `[0-9]+`, or `None` (`TIMELIB_UNSET`) when there are no digits.
fn read_number(b: &[u8], at: &mut usize) -> Option<i64> {
    let start = *at;
    let mut acc: i64 = 0;
    while *at < b.len() && b[*at].is_ascii_digit() {
        acc = acc * 10 + i64::from(b[*at] - b'0');
        *at += 1;
    }
    (*at > start).then_some(acc)
}

/// `[+-]? hh (:mm (:ss)?)?` as seconds EAST of UTC (a POSIX offset is
/// written west-positive, so the sign flips).
fn read_offset(b: &[u8], at: &mut usize) -> Option<i64> {
    let bias = match b.get(*at) {
        Some(b'+') => {
            *at += 1;
            1
        }
        Some(b'-') => {
            *at += 1;
            -1
        }
        _ => 1,
    };
    let hours = read_number(b, at)?;
    let mut minutes = 0;
    let mut seconds = 0;
    if b.get(*at) == Some(&b':') {
        *at += 1;
        minutes = read_number(b, at)?;
    }
    if b.get(*at) == Some(&b':') {
        *at += 1;
        seconds = read_number(b, at)?;
    }
    Some(-bias * (hours * 3600 + minutes * 60 + seconds))
}

/// `(Jn | n | Mm.w.d) ( /time )?`, the time defaulting to 02:00.
fn read_transition_spec(b: &[u8], at: &mut usize) -> Option<TransitionSpec> {
    let kind = if b.get(*at) == Some(&b'M') {
        *at += 1;
        let month = read_number(b, at)?;
        if b.get(*at) != Some(&b'.') {
            return None;
        }
        *at += 1;
        let week = read_number(b, at)?;
        if b.get(*at) != Some(&b'.') {
            return None;
        }
        *at += 1;
        let dow = read_number(b, at)?;
        SpecKind::Mwd { month, week, dow }
    } else if b.get(*at) == Some(&b'J') {
        *at += 1;
        SpecKind::JulianNoFeb29(read_number(b, at)?)
    } else {
        SpecKind::JulianFeb29(read_number(b, at)?)
    };
    let mut hour = 2 * 3600;
    if b.get(*at) == Some(&b'/') {
        *at += 1;
        hour = -read_offset(b, at)?;
    }
    Some(TransitionSpec { kind, hour })
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// `calc_transition`: seconds from the start of `year` to the rule's day.
/// C's truncating `/` and `%`, which Rust's integer operators share.
fn calc_transition(spec: &TransitionSpec, year: i64) -> i64 {
    const MONTH_LENGTHS: [[i64; 12]; 2] = [
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    ];
    let leap = usize::from(is_leap(year));
    match spec.kind {
        SpecKind::JulianNoFeb29(days) => {
            let mut value = days - 1;
            if leap == 1 && days >= 60 {
                value += 1;
            }
            value * SECS_PER_DAY
        }
        SpecKind::JulianFeb29(days) => days * SECS_PER_DAY,
        SpecKind::Mwd { month, week, dow } => {
            // Zeller's congruence for the weekday of the month's first day.
            let m1 = (month + 9) % 12 + 1;
            let yy0 = if month <= 2 { year - 1 } else { year };
            let (yy1, yy2) = (yy0 / 100, yy0 % 100);
            let mut first = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
            if first < 0 {
                first += 7;
            }
            let mut d = dow - first;
            if d < 0 {
                d += 7;
            }
            for _ in 1..week {
                if d + 7 >= MONTH_LENGTHS[leap][(month - 1) as usize] {
                    break;
                }
                d += 7;
            }
            let before: i64 = MONTH_LENGTHS[leap][..(month - 1) as usize].iter().sum();
            (d + before) * SECS_PER_DAY
        }
    }
}

/// The proleptic Gregorian year of `ts` UTC seconds, as
/// `timelib_unixtime2gmt` finds it (Hinnant's `civil_from_days`).
fn utc_year(ts: i64) -> i64 {
    let z = ts.div_euclid(SECS_PER_DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + i64::from(mp >= 10)
}

/// `timelib_ts_at_start_of_year`.
fn ts_at_start_of_year(year: i64) -> i64 {
    let count_leap_years = |y: i64| {
        let y = y - 1;
        y / 4 - y / 100 + y / 400
    };
    SECS_PER_DAY * ((year - 1970) * 365 + count_leap_years(year) - count_leap_years(1970))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset(name: &str, ts: i64) -> i64 {
        zone(name).unwrap().offset_info(ts).unwrap().0.offset
    }

    /// Every zone in the database parses, as mongod requires at startup.
    #[test]
    fn every_zone_parses() {
        let names: Vec<&str> = entries().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 597);
        for n in names {
            assert!(zone(n).is_some(), "{n}");
        }
    }

    /// Names match exactly, as mongod's map does (8.2.11, 2026-10-22).
    #[test]
    fn names_are_mongods() {
        for n in [
            "America/New_York",
            "Factory",
            "Zulu",
            "UTC",
            "EST",
            "Europe/Kiev",
        ] {
            assert!(is_zone(n), "{n}");
        }
        for n in [
            "america/new_york",
            "factory",
            "America/Coyhaique",
            "US/Pacific-New",
        ] {
            assert!(!is_zone(n), "{n}");
        }
    }

    /// Values measured on mongod 8.2.11 (2026-10-22), with `%z`.
    #[test]
    fn offsets_are_mongods() {
        // 2100-07-01 and 9000-07-01, 12:00Z: daylight saving continues.
        assert_eq!(offset("America/New_York", 4_118_126_400), -4 * 3600);
        assert_eq!(offset("America/New_York", 221_861_073_600), -4 * 3600);
        // 1900-07-01T12:00Z: backzone history.
        assert_eq!(offset("Europe/Amsterdam", -2_193_307_200), 19 * 60 + 32);
        // 2025-07-01T12:00Z: Paraguay before 2025a's permanent -03.
        assert_eq!(offset("America/Asuncion", 1_751_371_200), -4 * 3600);
    }
}
