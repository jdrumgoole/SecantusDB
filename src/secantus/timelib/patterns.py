"""The re2c definitions and rules of timelib 2022.13's ``parse_date.re``, as
vendored by mongod 8.2.11, translated to Python ``re`` BYTE patterns.

Copyright (c) 2015-2023 Derick Rethans, (c) 2018 MongoDB, Inc. MIT License
(``LICENSE-timelib.rst`` in this package). Translation for SecantusDB, from the
Rust server's ``crates/secantus-core/src/timelib/patterns.rs`` -- the same
table, so the two read side by side.

Translation rules, applied one-for-one: a re2c ``'single-quoted'`` literal is
case-insensitive -> ``(?i:...)``; a ``"double-quoted"`` one is case-sensitive;
``[\\000-\\377]`` -> ``[\\x00-\\xFF]``. Patterns are compiled over BYTES (re2c
scans bytes, and ``(?i)`` over bytes folds ASCII only, as re2c does).
"""

from __future__ import annotations

import re

#: ``(name, pattern)``, where ``{other}`` refers to an earlier definition. Same
#: names and order as ``parse_date.re``.
DEFINITIONS: tuple[tuple[str, str], ...] = (
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
    ("tz", r"(?:\(?[A-Za-z]{1,6}\)?|[A-Z][a-z]+(?:[_/\-][A-Za-z]+)+)"),
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
    ("dayofyear", r"(?:00[1-9]|0[1-9][0-9]|[1-2][0-9][0-9]|3[0-5][0-9]|36[0-6])"),
    ("weekofyear", r"(?:0[1-9]|[1-4][0-9]|5[0-3])"),
    ("monthlz", r"(?:0[0-9]|1[0-2])"),
    ("daylz", r"(?:0[0-9]|[1-2][0-9]|3[01])"),
    ("dayfulls", r"(?i:sundays|mondays|tuesdays|wednesdays|thursdays|fridays|saturdays)"),
    ("dayfull", r"(?i:sunday|monday|tuesday|wednesday|thursday|friday|saturday)"),
    ("dayabbr", r"(?i:sun|mon|tue|wed|thu|fri|sat|sun)"),
    ("dayspecial", r"(?i:weekday|weekdays)"),
    ("daytext", r"(?:{dayfulls}|{dayfull}|{dayabbr}|{dayspecial})"),
    (
        "monthfull",
        r"(?i:january|february|march|april|may|june|july|august|september|october|november|december)",
    ),
    ("monthabbr", r"(?i:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)"),
    ("monthroman", r"(?:I|II|III|IV|V|VI|VII|VIII|IX|X|XI|XII)"),
    ("monthtext", r"(?:{monthfull}|{monthabbr}|{monthroman})"),
    ("timetiny12", r"{hour12}(?:{space})?{meridian}"),
    ("timeshort12", r"{hour12}[:.]{minutelz}(?:{space})?{meridian}"),
    ("timelong12", r"{hour12}[:.]{minute}[:.]{secondlz}(?:{space})?{meridian}"),
    ("timetiny24", r"(?i:t){hour24}"),
    ("timeshort24", r"(?i:t)?{hour24}[:.]{minute}"),
    ("timelong24", r"(?i:t)?{hour24}[:.]{minute}[:.]{second}"),
    ("iso8601long", r"(?i:t)?{hour24}[:.]{minute}[:.]{second}{frac}"),
    (
        "iso8601normtz",
        r"(?i:t)?{hour24}[:.]{minute}[:.]{secondlz}(?:{space})?(?:{tzcorrection}|{tz})",
    ),
    ("gnunocolon", r"(?i:t)?{hour24lz}{minutelz}"),
    ("iso8601nocolon", r"(?i:t)?{hour24lz}{minutelz}{secondlz}"),
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
    ("datetextual", r"{monthtext}[ .\t\-]*{day}[,.stndrh\t ]+{year}"),
    ("datenoyear", r"{monthtext}[ .\t\-]*{day}(?:[,.stndrh\t ]+|\x00)"),
    ("datenoyearrev", r"{day}[ .\t\-]*{monthtext}"),
    ("datenocolon", r"{year4}{monthlz}{daylz}"),
    (
        "soap",
        r"{year4}-{monthlz}-{daylz}T{hour24lz}:{minutelz}:{secondlz}{frac}(?:{tzcorrection})?",
    ),
    ("xmlrpc", r"{year4}{monthlz}{daylz}T{hour24}:{minutelz}:{secondlz}"),
    ("xmlrpcnocolon", r"{year4}{monthlz}{daylz}(?i:t){hour24}{minutelz}{secondlz}"),
    ("wddx", r"{year4}-{month}-{day}T{hour24}:{minute}:{second}"),
    ("pgydotd", r"{year4}[.\-]?{dayofyear}"),
    ("pgtextshort", r"{monthabbr}-{daylz}-{year}"),
    ("pgtextreverse", r"{year}-{monthabbr}-{daylz}"),
    ("mssqltime", r"{hour12}:{minutelz}:{secondlz}[:.][0-9]+{meridian}"),
    ("isoweekday", r"{year4}-?W{weekofyear}-?[0-7]"),
    ("isoweek", r"{year4}-?W{weekofyear}"),
    ("exif", r"{year4}:{monthlz}:{daylz} {hour24lz}:{minutelz}:{secondlz}"),
    ("firstdayof", r"(?i:first day of)"),
    ("lastdayof", r"(?i:last day of)"),
    ("backof", r"(?i:back of ){hour24}(?:(?:{space})?{meridian})?"),
    ("frontof", r"(?i:front of ){hour24}(?:(?:{space})?{meridian})?"),
    ("clf", r"{day}/{monthabbr}/{year4}:{hour24lz}:{minutelz}:{secondlz}{space}{tzcorrection}"),
    ("timestamp", r"@-?[0-9]+"),
    ("timestampms", r"@-?[0-9]+\.[0-9]{0,6}"),
    ("dateshortwithtimeshort12", r"{datenoyear}{timeshort12}"),
    ("dateshortwithtimelong12", r"{datenoyear}{timelong12}"),
    ("dateshortwithtimeshort", r"{datenoyear}{timeshort24}"),
    ("dateshortwithtimelong", r"{datenoyear}{timelong24}"),
    ("dateshortwithtimelongtz", r"{datenoyear}{iso8601normtz}"),
    (
        "reltextnumber",
        r"(?i:first|second|third|fourth|fifth|sixth|seventh|eight|eighth|ninth|tenth|eleventh|twelfth)",
    ),
    ("reltexttext", r"(?i:next|last|previous|this)"),
    (
        "reltextunit",
        r"(?:(?i:ms)|\xC2\xB5(?i:s)|(?:(?i:msec|millisecond)|\xC2\xB5(?i:sec)|(?i:microsecond|usec|sec|second|min|minute|hour|day|fortnight|forthnight|month|year))(?i:s)?|(?i:weeks)|{daytext})",
    ),
    ("relnumber", r"(?:[+\-]*[ \t]*[0-9]{1,13})"),
    ("relative", r"{relnumber}(?:{space})?(?:{reltextunit}|(?i:week))"),
    ("relativetext", r"(?:{reltextnumber}|{reltexttext}){space}{reltextunit}"),
    ("relativetextweek", r"{reltexttext}{space}(?i:week)"),
    (
        "weekdayof",
        r"(?:{reltextnumber}|{reltexttext}){space}(?:{dayfulls}|{dayfull}|{dayabbr}){space}(?i:of)",
    ),
)

#: The scanner's rules, in ``parse_date.re`` order. ORDER IS SEMANTIC: re2c
#: takes the longest match, and on a tie the rule that comes first. Each entry
#: is the rule's alternatives (definition names, or a raw pattern prefixed
#: ``=``).
RULES: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("Yesterday", (r"=(?i:yesterday)",)),
    ("Now", (r"=(?i:now)",)),
    ("Noon", (r"=(?i:noon)",)),
    (
        "MidnightToday",
        (
            r"=(?i:midnight)",
            r"=(?i:today)",
        ),
    ),
    ("Tomorrow", (r"=(?i:tomorrow)",)),
    ("Timestamp", (r"timestamp",)),
    ("TimestampMs", (r"timestampms",)),
    (
        "FirstLastDayOf",
        (
            r"firstdayof",
            r"lastdayof",
        ),
    ),
    (
        "BackFrontOf",
        (
            r"backof",
            r"frontof",
        ),
    ),
    ("WeekdayOf", (r"weekdayof",)),
    (
        "Time12",
        (
            r"timetiny12",
            r"timeshort12",
            r"timelong12",
        ),
    ),
    ("MssqlTime", (r"mssqltime",)),
    (
        "Time24",
        (
            r"timetiny24",
            r"timeshort24",
            r"timelong24",
            r"iso8601long",
        ),
    ),
    ("GnuNoColon", (r"gnunocolon",)),
    ("Iso8601NoColon", (r"iso8601nocolon",)),
    (
        "American",
        (
            r"americanshort",
            r"american",
        ),
    ),
    (
        "IsoDate4",
        (
            r"iso8601date4",
            r"iso8601dateslash",
            r"dateslash",
        ),
    ),
    ("IsoDate2", (r"iso8601date2",)),
    ("IsoDateX", (r"iso8601datex",)),
    ("GnuDateShorter", (r"gnudateshorter",)),
    ("GnuDateShort", (r"gnudateshort",)),
    ("DateFull", (r"datefull",)),
    ("PointedDate4", (r"pointeddate4",)),
    ("PointedDate2", (r"pointeddate2",)),
    ("DateNoDay", (r"datenoday",)),
    ("DateNoDayRev", (r"datenodayrev",)),
    (
        "DateTextual",
        (
            r"datetextual",
            r"datenoyear",
        ),
    ),
    ("DateNoYearRev", (r"datenoyearrev",)),
    ("DateNoColon", (r"datenocolon",)),
    (
        "XmlRpc",
        (
            r"xmlrpc",
            r"xmlrpcnocolon",
            r"soap",
            r"wddx",
            r"exif",
        ),
    ),
    ("PgYdotd", (r"pgydotd",)),
    ("IsoWeekDay", (r"isoweekday",)),
    ("IsoWeek", (r"isoweek",)),
    ("PgTextShort", (r"pgtextshort",)),
    ("PgTextReverse", (r"pgtextreverse",)),
    ("Clf", (r"clf",)),
    ("Year4", (r"year4",)),
    ("Ago", (r"ago",)),
    ("DayText", (r"daytext",)),
    ("RelativeTextWeek", (r"relativetextweek",)),
    ("RelativeText", (r"relativetext",)),
    (
        "MonthText",
        (
            r"monthfull",
            r"monthabbr",
        ),
    ),
    (
        "Tz",
        (
            r"tzcorrection",
            r"tz",
        ),
    ),
    (
        "DateShortWithTime12",
        (
            r"dateshortwithtimeshort12",
            r"dateshortwithtimelong12",
        ),
    ),
    (
        "DateShortWithTime24",
        (
            r"dateshortwithtimeshort",
            r"dateshortwithtimelong",
            r"dateshortwithtimelongtz",
        ),
    ),
    ("Relative", (r"relative",)),
    ("DotComma", (r"=[.,]",)),
    ("Space", (r"space",)),
    (
        "NulNewline",
        (
            r"=\x00",
            r"=\n",
        ),
    ),
    ("Any", (r"any",)),
)

_REF = re.compile(r"\{([a-z][a-z0-9]*)\}")
_DEFS = dict(DEFINITIONS)


def expand(pattern: str) -> str:
    """Expand ``{name}`` references against the definitions.

    A ``{`` followed by a lowercase letter is a reference; one followed by a
    digit is a quantifier (``[0-9]{1,4}``) and is left alone.
    """
    for _ in range(100_000):
        m = _REF.search(pattern)
        if m is None:
            return pattern
        pattern = f"{pattern[: m.start()]}(?:{_DEFS[m.group(1)]}){pattern[m.end() :]}"
    raise RuntimeError(f"re2c definition expansion did not terminate: {pattern}")


def compiled_rules() -> list[tuple[str, re.Pattern[bytes]]]:
    """One compiled byte pattern per rule, in rule order."""
    out = []
    for rule, alts in RULES:
        parts = [
            f"(?:{expand(a[1:])})" if a.startswith("=") else f"(?:{expand('{' + a + '}')})"
            for a in alts
        ]
        out.append((rule, re.compile("|".join(parts).encode("latin-1"))))
    return out
