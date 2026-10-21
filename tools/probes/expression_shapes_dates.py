"""Date and arithmetic expression shapes for the uncovered lines of
crates/secantus-core/src/expressions.rs: op_trig (decimal zero / tiny / huge
operands), $exp, $pow, $round / $trunc (place validation, negative places on
integers, doubles), $dateDiff, $dateTrunc, $dateAdd / $dateSubtract,
$dateFromString (named zones, offsets, DST gaps and overlaps), $dateToString
(every format specifier, years outside 1000-9999, onNull, format
expressions), $dateFromParts, $dateToParts and the date-part operators.

Constant and field-path forms are paired for error cases, because mongod
validates constant arguments while optimising the pipeline ("Failed to
optimize pipeline :: caused by ::") and reports others at execution.
Probed against mongod 8.2.11, 2026-10-21.
"""

import datetime

from bson import Decimal128, Int64

D = datetime.datetime
DEC = Decimal128
NAN = float("nan")
INF = float("inf")
C = []


def add(label, expr, doc=None):
    C.append((label, expr) if doc is None else (label, expr, doc))


def both(label, op, v):
    """The constant form and the field-path form of a one-argument operator."""
    add(f"{label} const", {op: {"$literal": v}})
    add(f"{label} field", {op: "$v"}, {"_id": 1, "v": v})


# -------------------------------------------------------------- trig
TRIG = [
    "$sin",
    "$cos",
    "$tan",
    "$asin",
    "$acos",
    "$atan",
    "$sinh",
    "$cosh",
    "$tanh",
    "$asinh",
    "$acosh",
    "$atanh",
]
for op in TRIG:
    for name, v in [
        ("decNaN", DEC("NaN")),
        ("decNegInf", DEC("-Infinity")),
        ("decNeg0", DEC("-0")),
    ]:
        both(f"trig {op} {name}", op, v)
    # A decimal zero keeps its exponent on mongod; tiny operands keep their value.
    for z in ["0E+5", "-0.00"]:
        add(f"trig {op} zero {z}", {op: {"$literal": DEC(z)}})
    for v in ["1E-100", "1E+1100", "1E+34"]:
        add(f"trig {op} {v}", {op: {"$literal": DEC(v)}})
add("trig $acos dec 1", {"$acos": {"$literal": DEC("1")}})

# -------------------------------------------------------------- $exp / $ln / $log10 / $sqrt / $log
for name, v in [
    ("decNaN", DEC("NaN")),
    ("decInf", DEC("Infinity")),
    ("decNegInf", DEC("-Infinity")),
    ("dec0", DEC("0")),
    ("decNeg0", DEC("-0")),
    ("dec1E10", DEC("1E+10")),
    ("decNeg1E10", DEC("-1E+10")),
    ("dbl710", 710.0),
    ("str", "a"),
]:
    both(f"exp {name}", "$exp", v)
for op in ["$sqrt", "$ln", "$log10"]:
    for name, v in [
        ("dec-1", DEC("-1")),
        ("dec0", DEC("0")),
        ("decNegInf", DEC("-Infinity")),
        ("dbl-0", -0.0),
        ("i64-1", Int64(-1)),
    ]:
        both(f"{op} {name}", op, v)
for name, a, b in [
    ("base1", 10, 1),
    ("dec base1", DEC("10"), DEC("1")),
    ("x-1", -1, 10),
    ("base nan", 10, NAN),
    ("base str", 2, "a"),
    ("null", None, 2),
]:
    add(f"$log {name} const", {"$log": [{"$literal": a}, {"$literal": b}]})
    add(f"$log {name} field", {"$log": ["$a", "$b"]}, {"_id": 1, "a": a, "b": b})

# -------------------------------------------------------------- $pow
for name, a, b in [
    ("0^-1", 0, -1),
    ("0^-1 dec", DEC("0"), DEC("-1")),
    ("0^-1.5", 0, -1.5),
    ("2^-1", 2, -1),
    ("1^-5", 1, -5),
    ("-1^-5", -1, -5),
    ("-1^-4", -1, -4),
    ("i64 1^-3", Int64(1), -3),
    ("1^i64min", 1, Int64(-(2**63))),
    ("2^31", 2, 31),
    ("2^63 i64", Int64(2), 63),
    ("2^64", 2, 64),
    ("2^inf", 2, INF),
    ("null exp", 2, None),
    ("str base", "a", 2),
    ("dec^dec", DEC("2"), DEC("100")),
    ("dec 10^6145", DEC("10"), 6145),
    ("-0.0^-1", -0.0, -1),
]:
    add(f"pow {name} const", {"$pow": [{"$literal": a}, {"$literal": b}]})
    add(f"pow {name} field", {"$pow": ["$a", "$b"]}, {"_id": 1, "a": a, "b": b})

# -------------------------------------------------------------- $round / $trunc
for op in ["$round", "$trunc"]:
    for name, v, p in [
        ("int -2 half", 1250, -2),
        ("int -2 half odd", 1350, -2),
        ("int -2 neg", -1351, -2),
        ("i64 -19", Int64(2**62), -19),
        ("i64 max -1", Int64(2**63 - 1), -1),
        ("i64 min -1", Int64(-(2**63)), -1),
        ("int32 max -1", 2147483647, -1),
        ("int32 min -1", -2147483648, -1),
        ("int32 -2147483646 -1", -2147483646, -1),
        ("place dbl", 1.2345, 2.0),
        ("place dbl frac", 1.2345, 2.5),
        ("place dec", 1.2345, DEC("2")),
        ("place dec nan", 1.5, DEC("NaN")),
        ("place nan", 1.5, NAN),
        ("place big", 1.5, 2**31),
        ("place 101", 1.5, 101),
        ("place -21", 1.5, -21),
        ("place str", 1.5, "a"),
        ("place null", 1.5, None),
        ("value null place 200", None, 200),
        ("value null place str", None, "a"),
        ("dbl 2.675 2", 2.675, 2),
        ("dbl 1.45 2", 1.45, 2),
        ("dbl 0.045 3", 0.045, 3),
        ("dbl 1e300 10", 1e300, 10),
        ("dbl max 1", 1.7976931348623157e308, 1),
        ("dbl 1e20 3", 1e20, 3),
        ("dbl 1e23 -20", 1e23, -20),
        ("dbl 99.995 20", 99.995, 20),
        ("dbl 1000000000000000.5 1", 1e15 + 0.5, 1),
        ("dec 1234.5678 -2", DEC("1234.5678"), -2),
        ("dec 1E+6144 -20", DEC("1E+6144"), -20),
        ("str", "a", 0),
        ("date", D(2020, 1, 1), 0),
    ]:
        add(f"{op} {name} const", {op: [{"$literal": v}, {"$literal": p}]})
        add(f"{op} {name} field", {op: ["$v", "$p"]}, {"_id": 1, "v": v, "p": p})
    add(f"{op} empty array", {op: []})
    add(f"{op} missing place field", {op: [2.5, "$missing"]})

# -------------------------------------------------------------- $dateDiff
UNITS = ["year", "quarter", "month", "week", "day", "hour", "minute", "second", "millisecond"]
for u in UNITS:
    add(f"dateDiff {u}", {"$dateDiff": {"startDate": "$t", "endDate": "$t2", "unit": u}})
    add(
        f"dateDiff {u} NY",
        {
            "$dateDiff": {
                "startDate": "$t",
                "endDate": "$t2",
                "unit": u,
                "timezone": "America/New_York",
            }
        },
    )
    add(
        f"dateDiff {u} pre1970",
        {"$dateDiff": {"startDate": D(1901, 12, 31, 23, 59), "endDate": D(1969, 1, 1), "unit": u}},
    )
for sow in ["MON", "friday", "xyz", 1, None]:
    for u in ["week", "day"]:
        add(
            f"dateDiff {u} sow {sow!r} const",
            {
                "$dateDiff": {
                    "startDate": D(2026, 1, 1),
                    "endDate": D(2026, 1, 20),
                    "unit": u,
                    "startOfWeek": sow,
                }
            },
        )
        add(
            f"dateDiff {u} sow {sow!r} field",
            {"$dateDiff": {"startDate": "$t", "endDate": "$t2", "unit": u, "startOfWeek": sow}},
        )
add(
    "dateDiff sow field null day",
    {
        "$dateDiff": {
            "startDate": D(2026, 7, 5),
            "endDate": D(2026, 7, 25),
            "unit": "day",
            "startOfWeek": "$w",
        }
    },
    {"_id": 1},
)
for name, spec in [
    ("null start", {"startDate": None, "endDate": "$t", "unit": "day"}),
    ("null unit", {"startDate": "$t", "endDate": "$t2", "unit": None}),
    ("str start", {"startDate": "2020-01-01", "endDate": "$t2", "unit": "day"}),
    ("int end field", {"startDate": "$t", "endDate": "$n", "unit": "day"}),
    ("oid start", {"startDate": "$oid", "endDate": "$t2", "unit": "day"}),
    ("ts start", {"startDate": "$ts", "endDate": "$t2", "unit": "day"}),
    ("bad unit", {"startDate": "$t", "endDate": "$t2", "unit": "days"}),
    ("bad unit null start", {"startDate": "$nul", "endDate": "$t2", "unit": "days"}),
    ("int unit field", {"startDate": "$t", "endDate": "$t2", "unit": "$n"}),
    ("bad tz", {"startDate": "$t", "endDate": "$t2", "unit": "day", "timezone": "Mars/Base"}),
    ("int tz field", {"startDate": "$t", "endDate": "$t2", "unit": "day", "timezone": "$n"}),
    (
        "tz dublin hour",
        {
            "startDate": D(2026, 3, 28, 23),
            "endDate": D(2026, 3, 29, 2),
            "unit": "hour",
            "timezone": "Europe/Dublin",
        },
    ),
    (
        "tz kathmandu hour",
        {
            "startDate": D(2026, 7, 1, 3, 10),
            "endDate": D(2026, 7, 1, 3, 20),
            "unit": "hour",
            "timezone": "Asia/Kathmandu",
        },
    ),
    ("far apart ms", {"startDate": D(1, 1, 1), "endDate": D(9999, 12, 31), "unit": "millisecond"}),
]:
    add(f"dateDiff {name}", {"$dateDiff": spec})

# -------------------------------------------------------------- $dateTrunc
for u in UNITS:
    add(f"dateTrunc {u} bin3", {"$dateTrunc": {"date": "$t", "unit": u, "binSize": 3}})
    add(
        f"dateTrunc {u} Dublin bin2",
        {
            "$dateTrunc": {
                "date": D(2026, 3, 29, 1, 30),
                "unit": u,
                "binSize": 2,
                "timezone": "Europe/Dublin",
            }
        },
    )
    add(
        f"dateTrunc {u} pre1970 bin5",
        {"$dateTrunc": {"date": D(1955, 7, 19, 13, 14, 15, 678000), "unit": u, "binSize": 5}},
    )
    add(
        f"dateTrunc {u} year1 bin7",
        {"$dateTrunc": {"date": D(1, 1, 1, 0, 0, 1), "unit": u, "binSize": 7}},
    )
    add(
        f"dateTrunc {u} bin 1e12",
        {"$dateTrunc": {"date": "$t", "unit": u, "binSize": Int64(10**12)}},
    )
for name, b in [
    ("0", 0),
    ("dbl 2.5", 2.5),
    ("dec", DEC("2")),
    ("str", "2"),
    ("null", None),
    ("1e20", 1e20),
    ("i64 2**62", Int64(2**62)),
]:
    for u in ["hour", "month"]:
        add(f"dateTrunc {u} bin {name}", {"$dateTrunc": {"date": "$t", "unit": u, "binSize": b}})
for sow in ["mon", "xyz", 1, None]:
    for u in ["week", "day"]:
        add(
            f"dateTrunc {u} sow {sow!r}",
            {"$dateTrunc": {"date": "$t", "unit": u, "startOfWeek": sow}},
        )
add(
    "dateTrunc sow field null day",
    {"$dateTrunc": {"date": D(2026, 7, 5, 5), "unit": "day", "startOfWeek": "$w"}},
    {"_id": 1},
)
for name, spec in [
    ("date str", {"date": "2020", "unit": "day"}),
    ("date oid", {"date": "$oid", "unit": "day"}),
    ("date ts", {"date": "$ts", "unit": "day"}),
    ("unit null", {"date": "$t", "unit": None}),
    ("unit bad", {"date": "$t", "unit": "fortnight"}),
    ("unit bad null date", {"date": "$nul", "unit": "fortnight"}),
    ("bin 0 null date", {"date": "$nul", "unit": "day", "binSize": 0}),
    ("unit int", {"date": "$t", "unit": 3}),
    ("NY gap hour", {"date": D(2026, 3, 8, 7, 30), "unit": "hour", "timezone": "America/New_York"}),
    (
        "NY overlap hour",
        {"date": D(2026, 11, 1, 6, 30), "unit": "hour", "timezone": "America/New_York"},
    ),
    (
        "Santiago midnight gap day",
        {"date": D(2026, 9, 6, 6), "unit": "day", "timezone": "America/Santiago"},
    ),
    (
        "Beirut midnight gap day",
        {"date": D(2026, 3, 29, 12), "unit": "day", "timezone": "Asia/Beirut"},
    ),
    (
        "Kathmandu hour bin5",
        {"date": D(2026, 7, 1, 12, 40), "unit": "hour", "binSize": 5, "timezone": "Asia/Kathmandu"},
    ),
    (
        "NY hour bin5",
        {
            "date": D(2026, 7, 1, 12, 40),
            "unit": "hour",
            "binSize": 5,
            "timezone": "America/New_York",
        },
    ),
]:
    add(f"dateTrunc {name}", {"$dateTrunc": spec})

# -------------------------------------------------------------- $dateAdd / $dateSubtract
for op in ["$dateAdd", "$dateSubtract"]:
    for u in UNITS:
        add(f"{op} {u} 13", {op: {"startDate": "$t", "unit": u, "amount": 13}})
        add(
            f"{op} {u} NY",
            {
                op: {
                    "startDate": D(2026, 3, 7, 12),
                    "unit": u,
                    "amount": 1,
                    "timezone": "America/New_York",
                }
            },
        )
    for name, start, u, amt in [
        ("jan31+1m", D(2026, 1, 31, 10), "month", 1),
        ("jan31+1m leap", D(2024, 1, 31), "month", 1),
        ("feb29+1y", D(2024, 2, 29), "year", 1),
        ("aug31+q", D(2026, 8, 31), "quarter", 1),
        ("pre1970 month", D(1960, 1, 31, 5), "month", 1),
        ("y1 -1y", D(1, 6, 1), "year", -1),
        ("y9999 +1d", D(9999, 12, 31), "day", 1),
        ("year 8000", D(2026, 1, 1), "year", Int64(8000)),
        ("ms 1e15", D(2026, 1, 1), "millisecond", Int64(10**15)),
        ("huge months", D(2026, 1, 1), "month", Int64(10**12)),
        ("i64 min ms", D(2026, 1, 1), "millisecond", Int64(-(2**63))),
    ]:
        add(f"{op} {name}", {op: {"startDate": start, "unit": u, "amount": amt}})
    for name, spec in [
        ("amount 1.5", {"startDate": "$t", "unit": "day", "amount": 1.5}),
        ("amount dec", {"startDate": "$t", "unit": "day", "amount": DEC("2")}),
        ("amount str field", {"startDate": "$t", "unit": "day", "amount": "$s"}),
        ("amount nan", {"startDate": "$t", "unit": "day", "amount": NAN}),
        ("start str", {"startDate": "2020-01-01", "unit": "day", "amount": 1}),
        ("start int field", {"startDate": "$n", "unit": "day", "amount": 1}),
        ("start oid", {"startDate": "$oid", "unit": "day", "amount": 1}),
        ("start ts", {"startDate": "$ts", "unit": "day", "amount": 1}),
        ("unit null", {"startDate": "$t", "unit": None, "amount": 1}),
        ("unit null amount bad", {"startDate": "$t", "unit": None, "amount": 1.5}),
        ("unit bad", {"startDate": "$t", "unit": "eon", "amount": 1}),
        ("unit bad amount null", {"startDate": "$t", "unit": "eon", "amount": None}),
        ("unit int field", {"startDate": "$t", "unit": "$n", "amount": 1}),
        ("tz null", {"startDate": "$t", "unit": "day", "amount": 1, "timezone": None}),
        (
            "NY land in gap day",
            {
                "startDate": D(2026, 3, 7, 7, 30),
                "unit": "day",
                "amount": 1,
                "timezone": "America/New_York",
            },
        ),
        (
            "NY land in gap month",
            {
                "startDate": D(2026, 2, 8, 7, 30),
                "unit": "month",
                "amount": 1,
                "timezone": "America/New_York",
            },
        ),
        (
            "NY overlap hour",
            {
                "startDate": D(2026, 11, 1, 5, 30),
                "unit": "hour",
                "amount": 1,
                "timezone": "America/New_York",
            },
        ),
        (
            "Dublin land in gap day",
            {
                "startDate": D(2026, 3, 28, 1, 30),
                "unit": "day",
                "amount": 1,
                "timezone": "Europe/Dublin",
            },
        ),
        (
            "Apia day",
            {
                "startDate": D(2011, 12, 29, 12),
                "unit": "day",
                "amount": 1,
                "timezone": "Pacific/Apia",
            },
        ),
    ]:
        add(f"{op} {name}", {op: spec})

# -------------------------------------------------------------- $dateFromString
for name, spec in [
    (
        "offset in string with tz",
        {"dateString": "2026-01-02T03:04:05+05:30", "timezone": "America/New_York"},
    ),
    (
        "abbr in string with tz",
        {"dateString": "2026-01-02 03:04:05 EST", "timezone": "Europe/Dublin"},
    ),
    (
        "zone id in string with tz",
        {"dateString": "2026-01-02 03:04:05 Europe/London", "timezone": "Asia/Tokyo"},
    ),
    ("tz +05:30", {"dateString": "2026-01-02T03:04:05", "timezone": "+05:30"}),
    ("tz -0800", {"dateString": "2026-01-02T03:04:05", "timezone": "-0800"}),
    ("tz NY summer", {"dateString": "2026-07-02T03:04:05", "timezone": "America/New_York"}),
    ("tz NY gap", {"dateString": "2026-03-08T02:30:00", "timezone": "America/New_York"}),
    ("tz NY overlap", {"dateString": "2026-11-01T01:30:00", "timezone": "America/New_York"}),
    ("tz Dublin gap", {"dateString": "2026-03-29T01:30:00", "timezone": "Europe/Dublin"}),
    ("tz Dublin overlap", {"dateString": "2026-10-25T01:30:00", "timezone": "Europe/Dublin"}),
    ("tz Sydney overlap", {"dateString": "2026-04-05T02:30:00", "timezone": "Australia/Sydney"}),
    ("tz Lord Howe", {"dateString": "2026-04-05T01:45:00", "timezone": "Australia/Lord_Howe"}),
    ("tz Apia", {"dateString": "2011-12-30T12:00:00", "timezone": "Pacific/Apia"}),
    ("tz NY 2100", {"dateString": "2100-06-01T00:00:00", "timezone": "America/New_York"}),
    ("tz bad", {"dateString": "2026-01-02", "timezone": "Not/AZone"}),
    ("tz int field", {"dateString": "2026-01-02", "timezone": "$n"}),
    ("format int", {"dateString": "2026-01-02", "format": 5}),
    ("format int field", {"dateString": "2026-01-02", "format": "$n"}),
    ("format null", {"dateString": "2026-01-02", "format": None}),
    ("format bad", {"dateString": "2026-01-02", "format": "%Q"}),
    (
        "format with tz",
        {"dateString": "02/01/2026 10", "format": "%d/%m/%Y %H", "timezone": "Asia/Tokyo"},
    ),
    (
        "format z with tz",
        {"dateString": "02/01/2026 +0100", "format": "%d/%m/%Y %z", "timezone": "Asia/Tokyo"},
    ),
    (
        "format mismatch onError",
        {"dateString": "2026-01-02", "format": "%d/%m/%Y", "onError": "bad"},
    ),
    ("ds null onNull", {"dateString": None, "onNull": "N"}),
    ("ds int onError", {"dateString": 5, "onError": "E"}),
    ("ds int field", {"dateString": "$n"}),
    ("ds null bad format", {"dateString": None, "format": 5}),
    ("ds year 10000", {"dateString": "10000-01-01"}),
    ("ds overflow", {"dateString": "292278994-01-01"}),
]:
    add(f"dateFromString {name}", {"$dateFromString": spec})

# -------------------------------------------------------------- $dateToString
FMTS = ["%j", "%w", "%u", "%G", "%V", "%U", "%b", "%B", "%z", "%Z", "%%", "%Q", "abc%", "%%%"]
for f in FMTS:
    add(f"dateToString {f!r}", {"$dateToString": {"date": "$t", "format": f}})
    add(
        f"dateToString {f!r} -0930",
        {"$dateToString": {"date": "$t", "format": f, "timezone": "-0930"}},
    )
    add(f"dateToString {f!r} year 999", {"$dateToString": {"date": D(999, 12, 31), "format": f}})
for i, dt in enumerate(
    [D(2021, 1, 1), D(2027, 1, 3), D(1, 1, 1), D(999, 12, 31), D(9999, 12, 31, 23, 59, 59)]
):
    add(f"dateToString default d{i}", {"$dateToString": {"date": dt}})
    add(
        f"dateToString default Tokyo d{i}",
        {"$dateToString": {"date": dt, "timezone": "Asia/Tokyo"}},
    )
    add(
        f"dateToString %G-W%V-%u %U d{i}", {"$dateToString": {"date": dt, "format": "%G-W%V-%u %U"}}
    )
for name, spec, doc in [
    ("format field", {"date": "$t", "format": "$f"}, {"_id": 1, "t": D(2026, 1, 2), "f": "%Y"}),
    ("format field int", {"date": "$t", "format": "$f"}, {"_id": 1, "t": D(2026, 1, 2), "f": 5}),
    ("format missing field", {"date": "$t", "format": "$nope"}, {"_id": 1, "t": D(2026, 1, 2)}),
    (
        "format concat",
        {"date": "$t", "format": {"$concat": ["%Y", "-", "%m"]}},
        {"_id": 1, "t": D(2026, 1, 2)},
    ),
    ("format null", {"date": "$t", "format": None}, None),
    ("date null onNull", {"date": None, "onNull": "N"}, None),
    ("date missing onNull", {"date": "$nope", "onNull": "N"}, None),
    ("date missing onNull missing", {"date": "$nope", "onNull": "$nope2"}, None),
    ("date null onNull expr", {"date": "$nope", "onNull": {"$concat": ["a", "b"]}}, None),
    ("date null format int", {"date": None, "format": 5}, None),
    ("date null format %Q", {"date": "$nul", "format": "%Q"}, None),
    ("tz year 1 -05", {"date": D(1, 1, 1), "timezone": "-05"}, None),
    ("tz year 9999 +14", {"date": D(9999, 12, 31, 23), "timezone": "+14"}, None),
    (
        "tz far future NY",
        {"date": D(2300, 7, 1), "format": "%H %z", "timezone": "America/New_York"},
        None,
    ),
    (
        "tz Amsterdam 1900",
        {"date": D(1900, 7, 1, 12), "format": "%H:%M:%S %z", "timezone": "Europe/Amsterdam"},
        None,
    ),
    (
        "tz Reykjavik 1930",
        {"date": D(1930, 7, 1, 12), "format": "%H:%M:%S %z", "timezone": "Atlantic/Reykjavik"},
        None,
    ),
    (
        "tz Stockholm 1945",
        {"date": D(1945, 7, 1, 12), "format": "%H:%M:%S %z", "timezone": "Europe/Stockholm"},
        None,
    ),
    (
        "tz LMT NY 1880",
        {"date": D(1880, 7, 1), "format": "%H:%M:%S %z", "timezone": "America/New_York"},
        None,
    ),
]:
    add(f"dateToString {name}", {"$dateToString": spec}, doc)

# ---------------------------------------------- date parts, $dateToParts, $dateFromParts
PARTS = ["$year", "$dayOfYear", "$dayOfWeek", "$week", "$isoWeek", "$isoWeekYear", "$isoDayOfWeek"]
for p in PARTS:
    for i, dt in enumerate(
        [D(2021, 1, 1), D(2027, 1, 3), D(1, 1, 1), D(9999, 12, 31, 23, 59, 59, 999000)]
    ):
        add(f"part {p} d{i}", {p: {"$literal": dt}})
    add(f"part {p} +1400", {p: {"date": "$t", "timezone": "+1400"}})
    add(f"part {p} ts", {p: "$ts"})
for name, spec in [
    ("iso8601 int", {"date": "$t", "iso8601": 1}),
    ("iso8601 null", {"date": "$t", "iso8601": None}),
    ("iso8601 int null date", {"date": "$nul", "iso8601": 1}),
    ("date oid", {"date": "$oid"}),
    ("date ts", {"date": "$ts"}),
    ("tz Dublin iso", {"date": "$t", "timezone": "Europe/Dublin", "iso8601": True}),
]:
    add(f"dateToParts {name}", {"$dateToParts": spec})
for name, spec in [
    ("month 13", {"year": 2026, "month": 13}),
    ("day -1", {"year": 2026, "month": 3, "day": -1}),
    ("year 0", {"year": 0}),
    ("iso week 53", {"isoWeekYear": 2026, "isoWeek": 53, "isoDayOfWeek": 7}),
    (
        "NY gap",
        {
            "year": 2026,
            "month": 3,
            "day": 8,
            "hour": 2,
            "minute": 30,
            "timezone": "America/New_York",
        },
    ),
    (
        "NY overlap",
        {
            "year": 2026,
            "month": 11,
            "day": 1,
            "hour": 1,
            "minute": 30,
            "timezone": "America/New_York",
        },
    ),
    (
        "London overlap",
        {"year": 2026, "month": 10, "day": 25, "hour": 1, "timezone": "Europe/London"},
    ),
    (
        "Sydney overlap",
        {
            "year": 2026,
            "month": 4,
            "day": 5,
            "hour": 2,
            "minute": 30,
            "timezone": "Australia/Sydney",
        },
    ),
    (
        "Sydney gap",
        {
            "year": 2026,
            "month": 10,
            "day": 4,
            "hour": 2,
            "minute": 30,
            "timezone": "Australia/Sydney",
        },
    ),
    (
        "Santiago gap",
        {"year": 2026, "month": 9, "day": 6, "minute": 30, "timezone": "America/Santiago"},
    ),
    (
        "Apia skipped day",
        {"year": 2011, "month": 12, "day": 30, "hour": 12, "timezone": "Pacific/Apia"},
    ),
    ("NY 2100", {"year": 2100, "month": 7, "day": 1, "timezone": "America/New_York"}),
]:
    add(f"dateFromParts {name}", {"$dateFromParts": spec})

# -------------------------------------------------------------- $add / $subtract with dates
for name, args in [
    ("date+dbl round", ["$t", 2.5]),
    ("date+dec half", ["$t", DEC("2.5")]),
    ("date+nan", ["$t", NAN]),
    ("date+1e20", ["$t", 1e20]),
    ("date+i64max", ["$t", Int64(2**63 - 1)]),
]:
    add(f"add {name}", {"$add": args})
for name, args in [
    ("date-date pre1970", [D(1900, 1, 1), D(2200, 1, 1)]),
    ("date-dbl", ["$t", 1.5]),
    ("date-i64min", ["$t", Int64(-(2**63))]),
]:
    add(f"subtract {name}", {"$subtract": args})

CASES = C
