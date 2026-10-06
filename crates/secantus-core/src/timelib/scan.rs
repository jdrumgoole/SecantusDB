//! `timelib_strtotime`: the scanner loop, the rule actions and their helpers,
//! ported from timelib 2022.13 `parse_date.re` (MIT; see `mod.rs`).
//!
//! re2c semantics are reproduced exactly: at each position the LONGEST match
//! over all rules wins, a tie goes to the rule listed first, and the input is
//! the trimmed string followed by NUL padding -- several rules (`meridian`,
//! `datenoyear`) consume the terminating NUL as part of a token.

use std::cell::RefCell;
use std::sync::OnceLock;

use regex_automata::hybrid::dfa::{Cache, DFA};
use regex_automata::nfa::thompson;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind};

use super::patterns::{expand, Rule, RULES};
use super::zones::{TzEntry, TIMEZONE_MAP};
use super::{
    Message, Parsed, Time, SPECIAL_DAY_OF_WEEK_IN_MONTH, SPECIAL_FIRST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_MONTH, SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH, SPECIAL_WEEKDAY, UNSET,
    ZONETYPE_ABBR, ZONETYPE_OFFSET,
};

const ERR_DOUBLE_TZ: i32 = 0x201;
const ERR_TZID_NOT_FOUND: i32 = 0x202;
const ERR_DOUBLE_TIME: i32 = 0x203;
const ERR_DOUBLE_DATE: i32 = 0x204;
const ERR_UNEXPECTED_CHARACTER: i32 = 0x205;
const ERR_EMPTY_STRING: i32 = 0x206;
const ERR_UNEXPECTED_DATA: i32 = 0x207;
const ERR_NUMBER_OUT_OF_RANGE: i32 = 0x226;
const WARN_DOUBLE_TZ: i32 = 0x101;
const WARN_INVALID_TIME: i32 = 0x102;
const WARN_INVALID_DATE: i32 = 0x103;

/// `MAX_ABBR_LEN`: `_POSIX_TZNAME_MAX`, 6 on Linux and macOS alike.
const MAX_ABBR_LEN: usize = 6;
/// NUL bytes after the string; more than any rule looks ahead.
const PADDING: usize = 64;

// The relunit kinds of `timelib_relunit_lookup`.
const MICROSEC: i64 = 9;
const SECOND: i64 = 1;
const MINUTE: i64 = 2;
const HOUR: i64 = 3;
const DAY: i64 = 4;
const MONTH: i64 = 5;
const YEAR: i64 = 6;
const WEEKDAY: i64 = 7;
const SPECIAL: i64 = 8;

const TIME_PART_DONT_KEEP: bool = false;
const TIME_PART_KEEP: bool = true;

/// `timelib_relunit_lookup`: (name, unit, multiplier).
const RELUNITS: &[(&[u8], i64, i64)] = &[
    (b"ms", MICROSEC, 1000),
    (b"msec", MICROSEC, 1000),
    (b"msecs", MICROSEC, 1000),
    (b"millisecond", MICROSEC, 1000),
    (b"milliseconds", MICROSEC, 1000),
    ("µs".as_bytes(), MICROSEC, 1),
    (b"usec", MICROSEC, 1),
    (b"usecs", MICROSEC, 1),
    ("µsec".as_bytes(), MICROSEC, 1),
    ("µsecs".as_bytes(), MICROSEC, 1),
    (b"microsecond", MICROSEC, 1),
    (b"microseconds", MICROSEC, 1),
    (b"sec", SECOND, 1),
    (b"secs", SECOND, 1),
    (b"second", SECOND, 1),
    (b"seconds", SECOND, 1),
    (b"min", MINUTE, 1),
    (b"mins", MINUTE, 1),
    (b"minute", MINUTE, 1),
    (b"minutes", MINUTE, 1),
    (b"hour", HOUR, 1),
    (b"hours", HOUR, 1),
    (b"day", DAY, 1),
    (b"days", DAY, 1),
    (b"week", DAY, 7),
    (b"weeks", DAY, 7),
    (b"fortnight", DAY, 14),
    (b"fortnights", DAY, 14),
    (b"forthnight", DAY, 14),
    (b"forthnights", DAY, 14),
    (b"month", MONTH, 1),
    (b"months", MONTH, 1),
    (b"year", YEAR, 1),
    (b"years", YEAR, 1),
    (b"mondays", WEEKDAY, 1),
    (b"monday", WEEKDAY, 1),
    (b"mon", WEEKDAY, 1),
    (b"tuesdays", WEEKDAY, 2),
    (b"tuesday", WEEKDAY, 2),
    (b"tue", WEEKDAY, 2),
    (b"wednesdays", WEEKDAY, 3),
    (b"wednesday", WEEKDAY, 3),
    (b"wed", WEEKDAY, 3),
    (b"thursdays", WEEKDAY, 4),
    (b"thursday", WEEKDAY, 4),
    (b"thu", WEEKDAY, 4),
    (b"fridays", WEEKDAY, 5),
    (b"friday", WEEKDAY, 5),
    (b"fri", WEEKDAY, 5),
    (b"saturdays", WEEKDAY, 6),
    (b"saturday", WEEKDAY, 6),
    (b"sat", WEEKDAY, 6),
    (b"sundays", WEEKDAY, 0),
    (b"sunday", WEEKDAY, 0),
    (b"sun", WEEKDAY, 0),
    (b"weekday", SPECIAL, SPECIAL_WEEKDAY),
    (b"weekdays", SPECIAL, SPECIAL_WEEKDAY),
];

/// `timelib_reltext_lookup`: (name, behavior, value).
const RELTEXT: &[(&[u8], i64, i64)] = &[
    (b"first", 0, 1),
    (b"next", 0, 1),
    (b"second", 0, 2),
    (b"third", 0, 3),
    (b"fourth", 0, 4),
    (b"fifth", 0, 5),
    (b"sixth", 0, 6),
    (b"seventh", 0, 7),
    (b"eight", 0, 8),
    (b"eighth", 0, 8),
    (b"ninth", 0, 9),
    (b"tenth", 0, 10),
    (b"eleventh", 0, 11),
    (b"twelfth", 0, 12),
    (b"last", 0, -1),
    (b"previous", 0, -1),
    (b"this", 1, 0),
];

/// `timelib_month_lookup`.
const MONTHS: &[(&[u8], i64)] = &[
    (b"jan", 1),
    (b"feb", 2),
    (b"mar", 3),
    (b"apr", 4),
    (b"may", 5),
    (b"jun", 6),
    (b"jul", 7),
    (b"aug", 8),
    (b"sep", 9),
    (b"sept", 9),
    (b"oct", 10),
    (b"nov", 11),
    (b"dec", 12),
    (b"i", 1),
    (b"ii", 2),
    (b"iii", 3),
    (b"iv", 4),
    (b"v", 5),
    (b"vi", 6),
    (b"vii", 7),
    (b"viii", 8),
    (b"ix", 9),
    (b"x", 10),
    (b"xi", 11),
    (b"xii", 12),
    (b"january", 1),
    (b"february", 2),
    (b"march", 3),
    (b"april", 4),
    (b"may", 5),
    (b"june", 6),
    (b"july", 7),
    (b"august", 8),
    (b"september", 9),
    (b"october", 10),
    (b"november", 11),
    (b"december", 12),
];

// --- the matcher -----------------------------------------------------------

/// One pattern per rule, in rule order, so a pattern id IS a rule index.
fn dfa() -> &'static DFA {
    static DFA_CELL: OnceLock<DFA> = OnceLock::new();
    DFA_CELL.get_or_init(|| {
        let patterns: Vec<String> = RULES
            .iter()
            .map(|(_, alts)| {
                alts.iter()
                    .map(|a| match a.strip_prefix('=') {
                        Some(raw) => format!("(?:{})", expand(raw)),
                        None => format!("(?:{})", expand(&format!("{{{a}}}"))),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        DFA::builder()
            .configure(DFA::config().match_kind(MatchKind::All))
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .thompson(thompson::Config::new().utf8(false))
            .build_many(&patterns)
            .expect("timelib scanner patterns compile")
    })
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(dfa().create_cache());
}

/// re2c's choice at the start of `hay`: the longest match over every rule,
/// the earliest rule on a tie. `None` only if nothing matches, which the
/// `any` rule makes impossible for non-empty input.
fn longest_match(hay: &[u8]) -> Option<(Rule, usize)> {
    let dfa = dfa();
    CACHE.with(|cache| {
        let cache = &mut *cache.borrow_mut();
        let input = Input::new(hay).anchored(Anchored::Yes);
        let mut sid = dfa.start_state_forward(cache, &input).ok()?;
        let mut best: Option<(usize, usize)> = None; // (length, rule index)
        let mut record = |sid, len: usize, cache: &Cache| {
            if let Some(rule) = (0..dfa.match_len(cache, sid))
                .map(|k| dfa.match_pattern(cache, sid, k).as_usize())
                .min()
            {
                if best.is_none_or(|(l, _)| len > l) {
                    best = Some((len, rule));
                }
            }
        };
        for (i, &b) in hay.iter().enumerate() {
            sid = dfa.next_state(cache, sid, b).ok()?;
            if sid.is_tagged() {
                if sid.is_match() {
                    // Match states are delayed one byte: this one ends at i.
                    record(sid, i, cache);
                }
                if sid.is_dead() || sid.is_quit() {
                    return best.map(|(len, r)| (RULES[r].0, len));
                }
            }
        }
        sid = dfa.next_eoi_state(cache, sid).ok()?;
        if sid.is_match() {
            record(sid, hay.len(), cache);
        }
        best.map(|(len, r)| (RULES[r].0, len))
    })
}

// --- the scanner -----------------------------------------------------------

struct Scanner {
    /// The trimmed string followed by NUL padding.
    buf: Vec<u8>,
    len: usize,
    tok: usize,
    time: Time,
    errors: Vec<Message>,
    warnings: Vec<Message>,
}

/// A token copy with C-string semantics: reading past the end yields NUL.
pub(super) struct Tok<'a> {
    pub(super) b: &'a [u8],
    pub(super) p: usize,
}

impl Tok<'_> {
    pub(super) fn at(&self, i: usize) -> u8 {
        self.b.get(i).copied().unwrap_or(0)
    }
    pub(super) fn cur(&self) -> u8 {
        self.at(self.p)
    }
}

fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `timelib_strtotime`.
pub(crate) fn strtotime(input: &[u8]) -> Parsed {
    let mut s = Scanner {
        buf: Vec::new(),
        len: 0,
        tok: 0,
        time: Time::unset(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    let (mut start, mut end) = (0usize, input.len());
    if !input.is_empty() {
        let mut e = input.len() - 1;
        while is_c_space(input[start]) && start < e {
            start += 1;
        }
        while is_c_space(input[e]) && e > start {
            e -= 1;
        }
        end = e + 1;
    }
    if input.is_empty() {
        // `tok` is NULL here, so the position and character are both 0.
        s.errors.push(Message {
            code: ERR_EMPTY_STRING,
            position: 0,
            character: 0,
            message: "Empty string",
        });
        return Parsed {
            time: s.time,
            errors: s.errors,
            warnings: s.warnings,
        };
    }
    s.buf.extend_from_slice(&input[start..end]);
    s.len = s.buf.len();
    s.buf.extend(std::iter::repeat_n(0u8, PADDING));

    let mut cursor = 0usize;
    loop {
        s.tok = cursor;
        // YYFILL: the buffer is the string plus YYMAXFILL bytes, so the scan
        // stops once the token would start past the terminating NUL.
        if s.tok > s.len {
            break;
        }
        let Some((rule, len)) = longest_match(&s.buf[cursor..]) else {
            break;
        };
        cursor += len.max(1);
        let token = s.buf[s.tok..cursor].to_vec();
        s.action(rule, &token);
    }

    // "funky checking" whether the parsed time and date were valid.
    if s.time.have_time != 0 && !valid_time(s.time.h, s.time.i, s.time.s) {
        s.add_warning(WARN_INVALID_TIME, "The parsed time was invalid");
    }
    if s.time.have_date && !valid_date(s.time.y, s.time.m, s.time.d) {
        s.add_warning(WARN_INVALID_DATE, "The parsed date was invalid");
    }
    Parsed {
        time: s.time,
        errors: s.errors,
        warnings: s.warnings,
    }
}

impl Scanner {
    fn char_at_tok(&self) -> u8 {
        self.buf.get(self.tok).copied().unwrap_or(0)
    }
    fn add_error(&mut self, code: i32, message: &'static str) {
        self.errors.push(Message {
            code,
            position: self.tok,
            character: self.char_at_tok(),
            message,
        });
    }
    fn add_warning(&mut self, code: i32, message: &'static str) {
        self.warnings.push(Message {
            code,
            position: self.tok,
            character: self.char_at_tok(),
            message,
        });
    }

    // The TIMELIB_* state macros. Each `have_*` returns false where the C
    // macro `return TIMELIB_ERROR`s out of the action.
    fn have_time(&mut self) -> bool {
        if self.time.have_time != 0 {
            self.add_error(ERR_DOUBLE_TIME, "Double time specification");
            return false;
        }
        self.time.have_time = 1;
        self.time.h = 0;
        self.time.i = 0;
        self.time.s = 0;
        self.time.us = 0;
        true
    }
    fn unhave_time(&mut self) {
        self.time.have_time = 0;
        self.time.h = 0;
        self.time.i = 0;
        self.time.s = 0;
        self.time.us = 0;
    }
    fn have_date(&mut self) -> bool {
        if self.time.have_date {
            self.add_error(ERR_DOUBLE_DATE, "Double date specification");
            return false;
        }
        self.time.have_date = true;
        true
    }
    fn unhave_date(&mut self) {
        self.time.have_date = false;
        self.time.d = 0;
        self.time.m = 0;
        self.time.y = 0;
    }
    fn have_relative(&mut self) {
        self.time.have_relative = true;
    }
    fn have_weekday_relative(&mut self) {
        self.time.have_relative = true;
        self.time.relative.have_weekday_relative = true;
    }
    fn have_special_relative(&mut self) {
        self.time.have_relative = true;
        self.time.relative.have_special_relative = true;
    }
    fn have_tz(&mut self) -> bool {
        if self.time.have_zone != 0 {
            if self.time.have_zone > 1 {
                self.add_error(ERR_DOUBLE_TZ, "Double timezone specification");
            } else {
                self.add_warning(WARN_DOUBLE_TZ, "Double timezone specification");
            }
            self.time.have_zone += 1;
            return false;
        }
        self.time.have_zone += 1;
        true
    }

    /// The action block of one rule. `token` is the matched text, which the C
    /// code copies into a NUL-terminated string and walks with `ptr`.
    fn action(&mut self, rule: Rule, token: &[u8]) {
        let mut p = Tok { b: token, p: 0 };
        let t = &mut p;
        match rule {
            Rule::Yesterday => {
                self.have_relative();
                self.unhave_time();
                self.time.relative.d = -1;
            }
            Rule::Now => {}
            Rule::Noon => {
                self.unhave_time();
                if !self.have_time() {
                    return;
                }
                self.time.h = 12;
            }
            Rule::MidnightToday => self.unhave_time(),
            Rule::Tomorrow => {
                self.have_relative();
                self.unhave_time();
                self.time.relative.d = 1;
            }
            Rule::Timestamp | Rule::TimestampMs => {
                self.have_relative();
                self.unhave_date();
                self.unhave_time();
                if !self.have_tz() {
                    return;
                }
                let is_negative = t.at(1) == b'-';
                let i = self.get_signed_nr(t, 24);
                let mut us = 0;
                if rule == Rule::TimestampMs {
                    let before = t.p;
                    us = self.get_signed_nr(t, 6);
                    us *= 10i64.pow((7 - (t.p - before)) as u32);
                    if is_negative {
                        us *= -1;
                    }
                }
                self.time.y = 1970;
                self.time.m = 1;
                self.time.d = 1;
                self.time.h = 0;
                self.time.i = 0;
                self.time.s = 0;
                self.time.us = 0;
                self.time.relative.s += i;
                if rule == Rule::TimestampMs {
                    self.time.relative.us = us;
                }
                self.time.zone_type = ZONETYPE_OFFSET;
                self.time.z = 0;
                self.time.dst = 0;
            }
            Rule::FirstLastDayOf => {
                self.have_relative();
                self.unhave_time();
                self.time.relative.first_last_day_of = if matches!(t.cur(), b'l' | b'L') {
                    SPECIAL_LAST_DAY_OF_MONTH
                } else {
                    SPECIAL_FIRST_DAY_OF_MONTH
                };
            }
            Rule::BackFrontOf => {
                self.unhave_time();
                if !self.have_time() {
                    return;
                }
                if t.cur() == b'b' {
                    self.time.h = get_nr(t, 2);
                    self.time.i = 15;
                } else {
                    self.time.h = get_nr(t, 2) - 1;
                    self.time.i = 45;
                }
                if t.cur() != 0 {
                    eat_spaces(t);
                    self.time.h += meridian(t, self.time.h);
                }
            }
            Rule::WeekdayOf => {
                self.have_relative();
                self.have_special_relative();
                let mut behavior = 0;
                let i = get_relative_text(t, &mut behavior);
                eat_spaces(t);
                if i > 0 {
                    self.time.relative.special_type = SPECIAL_DAY_OF_WEEK_IN_MONTH;
                    self.set_relative(t, i, 1, TIME_PART_DONT_KEEP);
                } else {
                    self.time.relative.special_type = SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH;
                    self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP);
                }
            }
            Rule::Time12 => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(t, 2);
                if matches!(t.cur(), b':' | b'.') {
                    self.time.i = get_nr(t, 2);
                    if matches!(t.cur(), b':' | b'.') {
                        self.time.s = get_nr(t, 2);
                    }
                }
                eat_spaces(t);
                self.time.h += meridian(t, self.time.h);
            }
            Rule::MssqlTime => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(t, 2);
                self.time.i = get_nr(t, 2);
                if matches!(t.cur(), b':' | b'.') {
                    self.time.s = get_nr(t, 2);
                    if matches!(t.cur(), b':' | b'.') {
                        self.time.us = get_frac_nr(t);
                    }
                }
                eat_spaces(t);
                self.time.h += meridian(t, self.time.h);
            }
            Rule::Time24 => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(t, 2);
                if matches!(t.cur(), b':' | b'.') {
                    self.time.i = get_nr(t, 2);
                    if matches!(t.cur(), b':' | b'.') {
                        self.time.s = get_nr(t, 2);
                        if t.cur() == b'.' {
                            self.time.us = get_frac_nr(t);
                        }
                    }
                }
                if t.cur() != 0 {
                    self.zone(t);
                }
            }
            Rule::GnuNoColon => match self.time.have_time {
                0 => {
                    self.time.h = get_nr(t, 2);
                    self.time.i = get_nr(t, 2);
                    self.time.s = 0;
                    self.time.have_time += 1;
                }
                1 => {
                    self.time.y = get_nr(t, 4);
                    self.time.have_time += 1;
                }
                _ => self.add_error(ERR_DOUBLE_TIME, "Double time specification"),
            },
            Rule::Iso8601NoColon => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(t, 2);
                self.time.i = get_nr(t, 2);
                self.time.s = get_nr(t, 2);
                if t.cur() != 0 {
                    self.zone(t);
                }
            }
            Rule::American => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
                if t.cur() == b'/' {
                    let (y, len) = get_nr_ex(t, 4);
                    self.time.y = process_year(y, len);
                }
            }
            Rule::IsoDate4 => {
                if !self.have_date() {
                    return;
                }
                self.time.y = self.get_signed_nr(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
            }
            Rule::IsoDate2 => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
                self.time.y = process_year(y, len);
            }
            Rule::IsoDateX => {
                if !self.have_date() {
                    return;
                }
                self.time.y = self.get_signed_nr(t, 19);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
            }
            Rule::GnuDateShorter => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = 1;
                self.time.y = process_year(y, len);
            }
            Rule::GnuDateShort => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
                self.time.y = process_year(y, len);
            }
            Rule::DateFull => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(t, 2);
                skip_day_suffix(t);
                self.time.m = get_month(t);
                let (y, len) = get_nr_ex(t, 4);
                self.time.y = process_year(y, len);
            }
            Rule::PointedDate4 => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(t, 2);
                self.time.m = get_nr(t, 2);
                self.time.y = get_nr(t, 4);
            }
            Rule::PointedDate2 => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(t, 2);
                self.time.m = get_nr(t, 2);
                let (y, len) = get_nr_ex(t, 2);
                self.time.y = process_year(y, len);
            }
            Rule::DateNoDay => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(t);
                let (y, len) = get_nr_ex(t, 4);
                self.time.d = 1;
                self.time.y = process_year(y, len);
            }
            Rule::DateNoDayRev => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.m = get_month(t);
                self.time.d = 1;
                self.time.y = process_year(y, len);
            }
            Rule::DateTextual => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(t);
                self.time.d = get_nr(t, 2);
                let (y, len) = get_nr_ex(t, 4);
                self.time.y = process_year(y, len);
            }
            Rule::DateNoYearRev => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(t, 2);
                skip_day_suffix(t);
                self.time.m = get_month(t);
            }
            Rule::DateNoColon => {
                if !self.have_date() {
                    return;
                }
                self.time.y = get_nr(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
            }
            Rule::XmlRpc => {
                if !self.have_time() || !self.have_date() {
                    return;
                }
                self.time.y = get_nr(t, 4);
                self.time.m = get_nr(t, 2);
                self.time.d = get_nr(t, 2);
                self.time.h = get_nr(t, 2);
                self.time.i = get_nr(t, 2);
                self.time.s = get_nr(t, 2);
                if t.cur() == b'.' {
                    self.time.us = get_frac_nr(t);
                    if t.cur() != 0 {
                        self.zone(t);
                    }
                }
            }
            Rule::PgYdotd => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.d = get_nr(t, 3);
                self.time.m = 1;
                self.time.y = process_year(y, len);
            }
            Rule::IsoWeekDay | Rule::IsoWeek => {
                if !self.have_date() {
                    return;
                }
                self.have_relative();
                self.time.y = get_nr(t, 4);
                let w = get_nr(t, 2);
                let d = if rule == Rule::IsoWeekDay {
                    get_nr(t, 1)
                } else {
                    1
                };
                self.time.m = 1;
                self.time.d = 1;
                self.time.relative.d = super::update::daynr_from_weeknr(self.time.y, w, d);
            }
            Rule::PgTextShort => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(t);
                self.time.d = get_nr(t, 2);
                let (y, len) = get_nr_ex(t, 4);
                self.time.y = process_year(y, len);
            }
            Rule::PgTextReverse => {
                if !self.have_date() {
                    return;
                }
                let (y, len) = get_nr_ex(t, 4);
                self.time.m = get_month(t);
                self.time.d = get_nr(t, 2);
                self.time.y = process_year(y, len);
            }
            Rule::Clf => {
                if !self.have_time() || !self.have_date() {
                    return;
                }
                self.time.d = get_nr(t, 2);
                self.time.m = get_month(t);
                self.time.y = get_nr(t, 4);
                self.time.h = get_nr(t, 2);
                self.time.i = get_nr(t, 2);
                self.time.s = get_nr(t, 2);
                eat_spaces(t);
                self.zone(t);
            }
            Rule::Year4 => self.time.y = get_nr(t, 4),
            Rule::Ago => {
                let r = &mut self.time.relative;
                r.y = -r.y;
                r.m = -r.m;
                r.d = -r.d;
                r.h = -r.h;
                r.i = -r.i;
                r.s = -r.s;
                r.weekday = -r.weekday;
                if r.weekday == 0 {
                    r.weekday = -7;
                }
                if r.have_special_relative && r.special_type == SPECIAL_WEEKDAY {
                    r.special_amount = -r.special_amount;
                }
            }
            Rule::DayText => {
                self.have_relative();
                self.have_weekday_relative();
                self.unhave_time();
                if let Some((_, _, multiplier)) = lookup_relunit(t) {
                    self.time.relative.weekday = multiplier;
                }
                if self.time.relative.weekday_behavior != 2 {
                    self.time.relative.weekday_behavior = 1;
                }
            }
            Rule::RelativeTextWeek => {
                self.have_relative();
                while t.cur() != 0 {
                    let mut behavior = 0;
                    let i = get_relative_text(t, &mut behavior);
                    eat_spaces(t);
                    self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP);
                    self.time.relative.weekday_behavior = 2;
                    if !self.time.relative.have_weekday_relative {
                        self.have_weekday_relative();
                        self.time.relative.weekday = 1;
                    }
                }
            }
            Rule::RelativeText => {
                self.have_relative();
                while t.cur() != 0 {
                    let mut behavior = 0;
                    let i = get_relative_text(t, &mut behavior);
                    eat_spaces(t);
                    self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP);
                }
            }
            Rule::MonthText => {
                if !self.have_date() {
                    return;
                }
                self.time.m = lookup_month(t);
            }
            Rule::Tz => {
                if !self.have_tz() {
                    return;
                }
                eat_spaces(t);
                self.zone(t);
            }
            Rule::DateShortWithTime12 | Rule::DateShortWithTime24 => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(t);
                self.time.d = get_nr(t, 2);
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(t, 2);
                self.time.i = get_nr(t, 2);
                if rule == Rule::DateShortWithTime12 {
                    if matches!(t.cur(), b':' | b'.') {
                        self.time.s = get_nr(t, 2);
                        if t.cur() == b'.' {
                            self.time.us = get_frac_nr(t);
                        }
                    }
                    self.time.h += meridian(t, self.time.h);
                } else {
                    if t.cur() == b':' {
                        self.time.s = get_nr(t, 2);
                        if t.cur() == b'.' {
                            self.time.us = get_frac_nr(t);
                        }
                    }
                    if t.cur() != 0 {
                        self.zone(t);
                    }
                }
            }
            Rule::Relative => {
                self.have_relative();
                while t.cur() != 0 {
                    let i = self.get_signed_nr(t, 24);
                    eat_spaces(t);
                    self.set_relative(t, i, 1, TIME_PART_KEEP);
                }
            }
            Rule::DotComma | Rule::Space | Rule::NulNewline => {}
            Rule::Any => self.add_error(ERR_UNEXPECTED_CHARACTER, "Unexpected character"),
        }
    }

    /// `timelib_parse_zone` at the call sites: store the offset and report a
    /// zone that is not in the database.
    fn zone(&mut self, t: &mut Tok) {
        let (z, not_found) = parse_zone(t, &mut self.time);
        self.time.z = z;
        if not_found {
            self.add_error(
                ERR_TZID_NOT_FOUND,
                "The timezone could not be found in the database",
            );
        }
    }

    /// `timelib_get_signed_nr`.
    fn get_signed_nr(&mut self, t: &mut Tok, max_length: usize) -> i64 {
        while !t.cur().is_ascii_digit() && t.cur() != b'+' && t.cur() != b'-' {
            if t.cur() == 0 {
                self.add_error(ERR_UNEXPECTED_DATA, "Found unexpected data");
                return 0;
            }
            t.p += 1;
        }
        let mut negative = false;
        while matches!(t.cur(), b'+' | b'-') {
            if t.cur() == b'-' {
                negative = !negative;
            }
            t.p += 1;
        }
        while !t.cur().is_ascii_digit() {
            if t.cur() == 0 {
                self.add_error(ERR_UNEXPECTED_DATA, "Found unexpected data");
                return 0;
            }
            t.p += 1;
        }
        let mut digits = String::new();
        while t.cur().is_ascii_digit() && digits.len() < max_length {
            digits.push(t.cur() as char);
            t.p += 1;
        }
        let signed = if negative {
            format!("-{digits}")
        } else {
            digits
        };
        match signed.parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                self.add_error(ERR_NUMBER_OUT_OF_RANGE, "Number out of range");
                0
            }
        }
    }

    /// `timelib_set_relative`.
    fn set_relative(&mut self, t: &mut Tok, amount: i64, behavior: i64, keep_time: bool) {
        let Some((_, unit, multiplier)) = lookup_relunit(t) else {
            return;
        };
        let r = &mut self.time.relative;
        let field = match unit {
            MICROSEC => Some(&mut r.us),
            SECOND => Some(&mut r.s),
            MINUTE => Some(&mut r.i),
            HOUR => Some(&mut r.h),
            DAY => Some(&mut r.d),
            MONTH => Some(&mut r.m),
            YEAR => Some(&mut r.y),
            _ => None,
        };
        if let Some(field) = field {
            // add_with_overflow: __builtin_saddll_overflow on the product.
            match field.checked_add(amount.wrapping_mul(multiplier)) {
                Some(v) => *field = v,
                None => {
                    *field = field.wrapping_add(amount.wrapping_mul(multiplier));
                    self.add_error(ERR_NUMBER_OUT_OF_RANGE, "Number out of range");
                }
            }
            return;
        }
        match unit {
            WEEKDAY => {
                self.have_weekday_relative();
                if !keep_time {
                    self.unhave_time();
                }
                let r = &mut self.time.relative;
                r.d += (if amount > 0 { amount - 1 } else { amount }) * 7;
                r.weekday = multiplier;
                r.weekday_behavior = behavior;
            }
            SPECIAL => {
                self.have_special_relative();
                if !keep_time {
                    self.unhave_time();
                }
                self.time.relative.special_type = multiplier;
                self.time.relative.special_amount = amount;
            }
            _ => {}
        }
    }
}

// --- helpers (free functions in parse_date.re) -----------------------------

/// `TIMELIB_PROCESS_YEAR`.
fn process_year(y: i64, length: usize) -> i64 {
    if y == UNSET || length >= 4 {
        y
    } else if y < 100 {
        if y < 70 {
            y + 2000
        } else {
            y + 1900
        }
    } else {
        y
    }
}

/// `timelib_meridian`.
fn meridian(t: &mut Tok, h: i64) -> i64 {
    let mut retval = 0;
    while !matches!(t.cur(), b'A' | b'a' | b'P' | b'p') {
        // strchr("AaPp", c) also finds the terminating NUL, which ends the
        // loop at the end of the token.
        if t.cur() == 0 {
            break;
        }
        t.p += 1;
    }
    if matches!(t.cur(), b'a' | b'A') {
        if h == 12 {
            retval = -12;
        }
    } else if h != 12 {
        retval = 12;
    }
    t.p += 1;
    if t.cur() == b'.' {
        t.p += 1;
    }
    if matches!(t.cur(), b'M' | b'm') {
        t.p += 1;
    }
    if t.cur() == b'.' {
        t.p += 1;
    }
    retval
}

/// `timelib_get_nr_ex`: the number and how many digits it had.
pub(super) fn get_nr_ex(t: &mut Tok, max_length: usize) -> (i64, usize) {
    while !t.cur().is_ascii_digit() {
        if t.cur() == 0 {
            return (UNSET, 0);
        }
        t.p += 1;
    }
    let begin = t.p;
    while t.cur().is_ascii_digit() && t.p - begin < max_length {
        t.p += 1;
    }
    let digits = std::str::from_utf8(&t.b[begin..t.p]).unwrap_or("0");
    (digits.parse().unwrap_or(0), t.p - begin)
}

/// `timelib_get_nr`.
pub(super) fn get_nr(t: &mut Tok, max_length: usize) -> i64 {
    get_nr_ex(t, max_length).0
}

/// `timelib_skip_day_suffix`.
fn skip_day_suffix(t: &mut Tok) {
    if is_c_space(t.cur()) {
        return;
    }
    let two = [
        t.cur().to_ascii_lowercase(),
        t.at(t.p + 1).to_ascii_lowercase(),
    ];
    if matches!(&two, b"nd" | b"rd" | b"st" | b"th") {
        t.p += 2;
    }
}

/// `timelib_get_frac_nr`: the fraction as microseconds.
fn get_frac_nr(t: &mut Tok) -> i64 {
    while t.cur() != b'.' && t.cur() != b':' && !t.cur().is_ascii_digit() {
        if t.cur() == 0 {
            return UNSET;
        }
        t.p += 1;
    }
    let begin = t.p;
    while matches!(t.cur(), b'.' | b':') || t.cur().is_ascii_digit() {
        t.p += 1;
    }
    let end = t.p;
    // strtod of everything after the first character, stopping at the first
    // byte that is not part of a number.
    let body = &t.b[begin + 1..end];
    let numeric: String = body
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == b'.')
        .map(|&c| c as char)
        .collect();
    let value: f64 = numeric.parse().unwrap_or(0.0);
    (value * 10f64.powi(7 - (end - begin) as i32)) as i64
}

/// `timelib_lookup_relative_text`.
fn lookup_relative_text(t: &mut Tok, behavior: &mut i64) -> i64 {
    let begin = t.p;
    while t.cur().is_ascii_alphabetic() {
        t.p += 1;
    }
    let word = &t.b[begin..t.p];
    let mut value = 0;
    for (name, b, v) in RELTEXT {
        if word.eq_ignore_ascii_case(name) {
            value = *v;
            *behavior = *b;
        }
    }
    value
}

/// `timelib_get_relative_text`.
fn get_relative_text(t: &mut Tok, behavior: &mut i64) -> i64 {
    while matches!(t.cur(), b' ' | b'\t' | b'-' | b'/') {
        t.p += 1;
    }
    lookup_relative_text(t, behavior)
}

/// `timelib_lookup_month`.
pub(super) fn lookup_month(t: &mut Tok) -> i64 {
    let begin = t.p;
    while t.cur().is_ascii_alphabetic() {
        t.p += 1;
    }
    let word = &t.b[begin..t.p];
    let mut value = 0;
    for (name, v) in MONTHS {
        if word.eq_ignore_ascii_case(name) {
            value = *v;
        }
    }
    value
}

/// `timelib_get_month`.
fn get_month(t: &mut Tok) -> i64 {
    while matches!(t.cur(), b' ' | b'\t' | b'-' | b'.' | b'/') {
        t.p += 1;
    }
    lookup_month(t)
}

/// `timelib_eat_spaces`, NBSP and NNBSP included.
fn eat_spaces(t: &mut Tok) {
    loop {
        if matches!(t.cur(), b' ' | b'\t') {
            t.p += 1;
        } else if t.cur() == 0xE2 && t.at(t.p + 1) == 0x80 && t.at(t.p + 2) == 0xAF {
            t.p += 3;
        } else if t.cur() == 0xC2 && t.at(t.p + 1) == 0xA0 {
            t.p += 2;
        } else {
            break;
        }
    }
}

/// `timelib_lookup_relunit`: the first table entry matching the word.
fn lookup_relunit(t: &mut Tok) -> Option<(&'static [u8], i64, i64)> {
    let begin = t.p;
    while !matches!(
        t.cur(),
        0 | b' ' | b',' | b'\t' | b';' | b':' | b'/' | b'.' | b'-' | b'(' | b')'
    ) {
        t.p += 1;
    }
    let word = &t.b[begin..t.p];
    RELUNITS
        .iter()
        .find(|(name, _, _)| word.eq_ignore_ascii_case(name))
        .copied()
}

/// `abbr_search` with `gmtoffset == -1`: the first entry with this name.
fn abbr_search(word: &[u8]) -> Option<&'static TzEntry> {
    static UTC: TzEntry = TzEntry {
        name: "utc",
        dst: 0,
        gmtoffset: 0,
        full: "UTC",
    };
    if word.eq_ignore_ascii_case(b"utc") || word.eq_ignore_ascii_case(b"gmt") {
        return Some(&UTC);
    }
    TIMEZONE_MAP
        .iter()
        .find(|e| word.eq_ignore_ascii_case(e.name.as_bytes()))
}

/// `timelib_lookup_abbr`: (offset, dst, word, found).
fn lookup_abbr(t: &mut Tok) -> (i64, i64, Vec<u8>, bool) {
    let begin = t.p;
    while t.cur().is_ascii_alphanumeric() || matches!(t.cur(), b'/' | b'_' | b'-' | b'+') {
        t.p += 1;
    }
    let word = t.b[begin..t.p].to_vec();
    if word.len() < MAX_ABBR_LEN {
        if let Some(e) = abbr_search(&word) {
            return (e.gmtoffset - e.dst * 3600, e.dst, word, true);
        }
    }
    (0, 0, word, false)
}

fn strtol_prefix(b: &[u8]) -> i64 {
    let digits: String = b
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .map(|&c| c as char)
        .collect();
    digits.parse().unwrap_or(0)
}

/// `timelib_parse_tz_cor`: (seconds, not_found).
fn parse_tz_cor(t: &mut Tok) -> (i64, bool) {
    let begin = t.p;
    while t.cur().is_ascii_digit() || t.cur() == b':' {
        t.p += 1;
    }
    let b = &t.b[begin..t.p];
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let hour = |s: &[u8]| strtol_prefix(s) * 3600;
    let min = |s: &[u8]| strtol_prefix(s) * 60;
    match b.len() {
        1 | 2 => (hour(b), false),
        3 | 4 => {
            if at(1) == b':' {
                (hour(b) + min(&b[2..]), false)
            } else if at(2) == b':' {
                (hour(b) + min(&b[3..]), false)
            } else {
                let tmp = strtol_prefix(b);
                ((tmp / 100) * 3600 + (tmp % 100) * 60, false)
            }
        }
        5 if at(2) == b':' => (hour(b) + min(&b[3..]), false),
        6 => {
            let tmp = strtol_prefix(b);
            (
                (tmp / 10000) * 3600 + ((tmp / 100) % 100) * 60 + tmp % 100,
                false,
            )
        }
        8 if at(2) == b':' && at(5) == b':' => {
            (hour(b) + min(&b[3..]) + strtol_prefix(&b[6..]), false)
        }
        _ => (0, true),
    }
}

/// `timelib_parse_zone` with mongod's `tz_get_wrapper`, which never finds a
/// zone identifier. Returns (offset, not_found).
pub(super) fn parse_zone(t: &mut Tok, time: &mut Time) -> (i64, bool) {
    while matches!(t.cur(), b' ' | b'\t' | b'(') {
        t.p += 1;
    }
    if t.cur() == b'G'
        && t.at(t.p + 1) == b'M'
        && t.at(t.p + 2) == b'T'
        && matches!(t.at(t.p + 3), b'+' | b'-')
    {
        t.p += 3;
    }
    let result = if matches!(t.cur(), b'+' | b'-') {
        let negative = t.cur() == b'-';
        t.p += 1;
        time.zone_type = ZONETYPE_OFFSET;
        time.dst = 0;
        let (v, not_found) = parse_tz_cor(t);
        (if negative { -v } else { v }, not_found)
    } else {
        let (offset, dst, word, found) = lookup_abbr(t);
        if found {
            time.zone_type = ZONETYPE_ABBR;
            time.dst = dst;
            time.tz_abbr = String::from_utf8_lossy(&word).to_ascii_uppercase();
        }
        (offset, !found)
    };
    while t.cur() == b')' {
        t.p += 1;
    }
    result
}

/// `timelib_valid_time`.
pub(crate) fn valid_time(h: i64, i: i64, s: i64) -> bool {
    (0..=23).contains(&h) && (0..=59).contains(&i) && (0..=59).contains(&s)
}

/// `timelib_valid_date`.
pub(crate) fn valid_date(y: i64, m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && d >= 1 && d <= super::update::days_in_month(y, m)
}
