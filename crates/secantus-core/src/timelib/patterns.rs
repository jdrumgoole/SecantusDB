//! The re2c definitions and rules of timelib 2022.13's `parse_date.re`, as
//! vendored by mongod 8.2.11, translated to `regex-automata` byte patterns.
//!
//! Copyright (c) 2015-2023 Derick Rethans, (c) 2018 MongoDB, Inc. MIT License
//! (`src/timelib/LICENSE-timelib.rst` in this crate). Translation for SecantusDB.
//!
//! Translation rules, applied one-for-one so the two can be read side by side:
//! a re2c `'single-quoted'` literal is case-insensitive -> `(?i:...)`; a
//! `"double-quoted"` one is case-sensitive; `[\000-\377]` -> `[\x00-\xFF]`.
//! Patterns are compiled in BYTE mode (no Unicode), as re2c scans bytes.

/// `(name, pattern)`, where `{other}` refers to an earlier definition. Same
/// names and order as `parse_date.re`.
pub(crate) const DEFINITIONS: &[(&str, &str)] = &[
    ("any", r"[\x00-\xFF]"),
    ("nbsp", r"\xC2\xA0"),
    ("nnbsp", r"\xE2\x80\xAF"),
    ("space", r"(?:[ \t]+|(?:{nbsp})+|(?:{nnbsp})+)"),
    ("frac", r"\.[0-9]+"),
    ("ago", r"(?i:ago)"),
    ("hour24", r"(?:[01]?[0-9]|2[0-4])"),
    ("hour24lz", r"(?:[01][0-9]|2[0-4])"),
    ("hour12", r"(?:0?[1-9]|1[0-2])"),
    ("minute", r"(?:[0-5]?[0-9])"),
    ("minutelz", r"(?:[0-5][0-9])"),
    ("second", r"(?:{minute}|60)"),
    ("secondlz", r"(?:{minutelz}|60)"),
    ("meridian", r"(?:[AaPp]\.?[Mm]\.?)[\x00\t ]"),
    (
        "tz",
        r"(?:\(?[A-Za-z]{1,6}\)?|[A-Z][a-z]+(?:[_/\-][A-Za-z]+)+)",
    ),
    (
        "tzcorrection",
        r"(?:GMT)?[+\-](?:(?:{hour24}(?::?{minute})?)|(?:{hour24lz}{minutelz}{secondlz})|(?:{hour24lz}:{minutelz}:{secondlz}))",
    ),
    ("daysuf", r"(?:st|nd|rd|th)"),
    ("month", r"(?:0?[0-9]|1[0-2])"),
    ("day", r"(?:(?:[0-2]?[0-9]|3[01]){daysuf}?)"),
    ("year", r"[0-9]{1,4}"),
    ("year2", r"[0-9]{2}"),
    ("year4", r"[0-9]{4}"),
    ("year4withsign", r"[+\-]?[0-9]{4}"),
    ("yearx", r"[+\-][0-9]{5,19}"),
    (
        "dayofyear",
        r"(?:00[1-9]|0[1-9][0-9]|[1-2][0-9][0-9]|3[0-5][0-9]|36[0-6])",
    ),
    ("weekofyear", r"(?:0[1-9]|[1-4][0-9]|5[0-3])"),
    ("monthlz", r"(?:0[0-9]|1[0-2])"),
    ("daylz", r"(?:0[0-9]|[1-2][0-9]|3[01])"),
    (
        "dayfulls",
        r"(?i:sundays|mondays|tuesdays|wednesdays|thursdays|fridays|saturdays)",
    ),
    (
        "dayfull",
        r"(?i:sunday|monday|tuesday|wednesday|thursday|friday|saturday)",
    ),
    ("dayabbr", r"(?i:sun|mon|tue|wed|thu|fri|sat|sun)"),
    ("dayspecial", r"(?i:weekday|weekdays)"),
    (
        "daytext",
        r"(?:{dayfulls}|{dayfull}|{dayabbr}|{dayspecial})",
    ),
    (
        "monthfull",
        r"(?i:january|february|march|april|may|june|july|august|september|october|november|december)",
    ),
    (
        "monthabbr",
        r"(?i:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)",
    ),
    ("monthroman", r"(?:I|II|III|IV|V|VI|VII|VIII|IX|X|XI|XII)"),
    ("monthtext", r"(?:{monthfull}|{monthabbr}|{monthroman})"),
    // Time formats
    ("timetiny12", r"{hour12}(?:{space})?{meridian}"),
    (
        "timeshort12",
        r"{hour12}[:.]{minutelz}(?:{space})?{meridian}",
    ),
    (
        "timelong12",
        r"{hour12}[:.]{minute}[:.]{secondlz}(?:{space})?{meridian}",
    ),
    ("timetiny24", r"(?i:t){hour24}"),
    ("timeshort24", r"(?i:t)?{hour24}[:.]{minute}"),
    ("timelong24", r"(?i:t)?{hour24}[:.]{minute}[:.]{second}"),
    (
        "iso8601long",
        r"(?i:t)?{hour24}[:.]{minute}[:.]{second}{frac}",
    ),
    (
        "iso8601normtz",
        r"(?i:t)?{hour24}[:.]{minute}[:.]{secondlz}(?:{space})?(?:{tzcorrection}|{tz})",
    ),
    ("gnunocolon", r"(?i:t)?{hour24lz}{minutelz}"),
    ("iso8601nocolon", r"(?i:t)?{hour24lz}{minutelz}{secondlz}"),
    // Date formats
    ("americanshort", r"{month}/{day}"),
    ("american", r"{month}/{day}/{year}"),
    ("iso8601dateslash", r"{year4}/{monthlz}/{daylz}/?"),
    ("dateslash", r"{year4}/{month}/{day}"),
    ("iso8601date4", r"{year4withsign}-{monthlz}-{daylz}"),
    ("iso8601date2", r"{year2}-{monthlz}-{daylz}"),
    ("iso8601datex", r"{yearx}-{monthlz}-{daylz}"),
    ("gnudateshorter", r"{year4}-{month}"),
    ("gnudateshort", r"{year}-{month}-{day}"),
    ("pointeddate4", r"{day}[.\t\-]{month}[.\-]{year4}"),
    ("pointeddate2", r"{day}[.\t]{month}\.{year2}"),
    ("datefull", r"{day}[ \t.\-]*{monthtext}[ \t.\-]*{year}"),
    ("datenoday", r"{monthtext}[ .\t\-]*{year4}"),
    ("datenodayrev", r"{year4}[ .\t\-]*{monthtext}"),
    (
        "datetextual",
        r"{monthtext}[ .\t\-]*{day}[,.stndrh\t ]+{year}",
    ),
    (
        "datenoyear",
        r"{monthtext}[ .\t\-]*{day}(?:[,.stndrh\t ]+|\x00)",
    ),
    ("datenoyearrev", r"{day}[ .\t\-]*{monthtext}"),
    ("datenocolon", r"{year4}{monthlz}{daylz}"),
    // Special formats
    (
        "soap",
        r"{year4}-{monthlz}-{daylz}T{hour24lz}:{minutelz}:{secondlz}{frac}(?:{tzcorrection})?",
    ),
    (
        "xmlrpc",
        r"{year4}{monthlz}{daylz}T{hour24}:{minutelz}:{secondlz}",
    ),
    (
        "xmlrpcnocolon",
        r"{year4}{monthlz}{daylz}(?i:t){hour24}{minutelz}{secondlz}",
    ),
    ("wddx", r"{year4}-{month}-{day}T{hour24}:{minute}:{second}"),
    ("pgydotd", r"{year4}[.\-]?{dayofyear}"),
    ("pgtextshort", r"{monthabbr}-{daylz}-{year}"),
    ("pgtextreverse", r"{year}-{monthabbr}-{daylz}"),
    (
        "mssqltime",
        r"{hour12}:{minutelz}:{secondlz}[:.][0-9]+{meridian}",
    ),
    ("isoweekday", r"{year4}-?W{weekofyear}-?[0-7]"),
    ("isoweek", r"{year4}-?W{weekofyear}"),
    (
        "exif",
        r"{year4}:{monthlz}:{daylz} {hour24lz}:{minutelz}:{secondlz}",
    ),
    ("firstdayof", r"(?i:first day of)"),
    ("lastdayof", r"(?i:last day of)"),
    (
        "backof",
        r"(?i:back of ){hour24}(?:(?:{space})?{meridian})?",
    ),
    (
        "frontof",
        r"(?i:front of ){hour24}(?:(?:{space})?{meridian})?",
    ),
    (
        "clf",
        r"{day}/{monthabbr}/{year4}:{hour24lz}:{minutelz}:{secondlz}{space}{tzcorrection}",
    ),
    ("timestamp", r"@-?[0-9]+"),
    ("timestampms", r"@-?[0-9]+\.[0-9]{0,6}"),
    ("dateshortwithtimeshort12", r"{datenoyear}{timeshort12}"),
    ("dateshortwithtimelong12", r"{datenoyear}{timelong12}"),
    ("dateshortwithtimeshort", r"{datenoyear}{timeshort24}"),
    ("dateshortwithtimelong", r"{datenoyear}{timelong24}"),
    ("dateshortwithtimelongtz", r"{datenoyear}{iso8601normtz}"),
    // Relative regexps
    (
        "reltextnumber",
        r"(?i:first|second|third|fourth|fifth|sixth|seventh|eight|eighth|ninth|tenth|eleventh|twelfth)",
    ),
    ("reltexttext", r"(?i:next|last|previous|this)"),
    // 'µs' etc.: re2c folds ASCII case only, so the µ (UTF-8 C2 B5) is literal.
    (
        "reltextunit",
        r"(?:(?i:ms)|\xC2\xB5(?i:s)|(?:(?i:msec|millisecond)|\xC2\xB5(?i:sec)|(?i:microsecond|usec|sec|second|min|minute|hour|day|fortnight|forthnight|month|year))(?i:s)?|(?i:weeks)|{daytext})",
    ),
    ("relnumber", r"(?:[+\-]*[ \t]*[0-9]{1,13})"),
    (
        "relative",
        r"{relnumber}(?:{space})?(?:{reltextunit}|(?i:week))",
    ),
    (
        "relativetext",
        r"(?:{reltextnumber}|{reltexttext}){space}{reltextunit}",
    ),
    ("relativetextweek", r"{reltexttext}{space}(?i:week)"),
    (
        "weekdayof",
        r"(?:{reltextnumber}|{reltexttext}){space}(?:{dayfulls}|{dayfull}|{dayabbr}){space}(?i:of)",
    ),
];

/// The scanner's rules, in `parse_date.re` order. ORDER IS SEMANTIC: re2c takes
/// the longest match, and on a tie the rule that comes first. Each entry is the
/// rule's alternatives (definition names, or a raw pattern prefixed `=`).
pub(crate) const RULES: &[(Rule, &[&str])] = &[
    (Rule::Yesterday, &["=(?i:yesterday)"]),
    (Rule::Now, &["=(?i:now)"]),
    (Rule::Noon, &["=(?i:noon)"]),
    (Rule::MidnightToday, &["=(?i:midnight)", "=(?i:today)"]),
    (Rule::Tomorrow, &["=(?i:tomorrow)"]),
    (Rule::Timestamp, &["timestamp"]),
    (Rule::TimestampMs, &["timestampms"]),
    (Rule::FirstLastDayOf, &["firstdayof", "lastdayof"]),
    (Rule::BackFrontOf, &["backof", "frontof"]),
    (Rule::WeekdayOf, &["weekdayof"]),
    (Rule::Time12, &["timetiny12", "timeshort12", "timelong12"]),
    (Rule::MssqlTime, &["mssqltime"]),
    (
        Rule::Time24,
        &["timetiny24", "timeshort24", "timelong24", "iso8601long"],
    ),
    (Rule::GnuNoColon, &["gnunocolon"]),
    (Rule::Iso8601NoColon, &["iso8601nocolon"]),
    (Rule::American, &["americanshort", "american"]),
    (
        Rule::IsoDate4,
        &["iso8601date4", "iso8601dateslash", "dateslash"],
    ),
    (Rule::IsoDate2, &["iso8601date2"]),
    (Rule::IsoDateX, &["iso8601datex"]),
    (Rule::GnuDateShorter, &["gnudateshorter"]),
    (Rule::GnuDateShort, &["gnudateshort"]),
    (Rule::DateFull, &["datefull"]),
    (Rule::PointedDate4, &["pointeddate4"]),
    (Rule::PointedDate2, &["pointeddate2"]),
    (Rule::DateNoDay, &["datenoday"]),
    (Rule::DateNoDayRev, &["datenodayrev"]),
    (Rule::DateTextual, &["datetextual", "datenoyear"]),
    (Rule::DateNoYearRev, &["datenoyearrev"]),
    (Rule::DateNoColon, &["datenocolon"]),
    (
        Rule::XmlRpc,
        &["xmlrpc", "xmlrpcnocolon", "soap", "wddx", "exif"],
    ),
    (Rule::PgYdotd, &["pgydotd"]),
    (Rule::IsoWeekDay, &["isoweekday"]),
    (Rule::IsoWeek, &["isoweek"]),
    (Rule::PgTextShort, &["pgtextshort"]),
    (Rule::PgTextReverse, &["pgtextreverse"]),
    (Rule::Clf, &["clf"]),
    (Rule::Year4, &["year4"]),
    (Rule::Ago, &["ago"]),
    (Rule::DayText, &["daytext"]),
    (Rule::RelativeTextWeek, &["relativetextweek"]),
    (Rule::RelativeText, &["relativetext"]),
    (Rule::MonthText, &["monthfull", "monthabbr"]),
    (Rule::Tz, &["tzcorrection", "tz"]),
    (
        Rule::DateShortWithTime12,
        &["dateshortwithtimeshort12", "dateshortwithtimelong12"],
    ),
    (
        Rule::DateShortWithTime24,
        &[
            "dateshortwithtimeshort",
            "dateshortwithtimelong",
            "dateshortwithtimelongtz",
        ],
    ),
    (Rule::Relative, &["relative"]),
    (Rule::DotComma, &["=[.,]"]),
    (Rule::Space, &["space"]),
    (Rule::NulNewline, &[r"=\x00", r"=\n"]),
    (Rule::Any, &["any"]),
];

/// One scanner rule -- one action block of `parse_date.re`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    Yesterday,
    Now,
    Noon,
    MidnightToday,
    Tomorrow,
    Timestamp,
    TimestampMs,
    FirstLastDayOf,
    BackFrontOf,
    WeekdayOf,
    Time12,
    MssqlTime,
    Time24,
    GnuNoColon,
    Iso8601NoColon,
    American,
    IsoDate4,
    IsoDate2,
    IsoDateX,
    GnuDateShorter,
    GnuDateShort,
    DateFull,
    PointedDate4,
    PointedDate2,
    DateNoDay,
    DateNoDayRev,
    DateTextual,
    DateNoYearRev,
    DateNoColon,
    XmlRpc,
    PgYdotd,
    IsoWeekDay,
    IsoWeek,
    PgTextShort,
    PgTextReverse,
    Clf,
    Year4,
    Ago,
    DayText,
    RelativeTextWeek,
    RelativeText,
    MonthText,
    Tz,
    DateShortWithTime12,
    DateShortWithTime24,
    Relative,
    DotComma,
    Space,
    NulNewline,
    Any,
}

/// Expand `{name}` references against the definitions (innermost first).
pub(crate) fn expand(pattern: &str) -> String {
    let mut out = pattern.to_string();
    // A `{` followed by a lowercase letter is a reference; one followed by a
    // digit is a regex quantifier (`[0-9]{1,4}`) and is left alone. Each pass
    // expands one reference; definitions only refer to earlier ones, so this
    // terminates, and the bound turns a typo into a panic, not a hang.
    for _ in 0..100_000 {
        let Some(start) = out.char_indices().find_map(|(i, c)| {
            (c == '{'
                && out[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase()))
            .then_some(i)
        }) else {
            return out;
        };
        let end = start + out[start..].find('}').expect("unterminated {name}");
        let name = &out[start + 1..end];
        let def = DEFINITIONS
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("unknown re2c definition {name}"))
            .1;
        out = format!("{}(?:{}){}", &out[..start], def, &out[end + 1..]);
    }
    panic!("re2c definition expansion did not terminate: {pattern}");
}
