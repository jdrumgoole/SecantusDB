"""`$toDate` date-string parsing against mongod: values, and which strings parse.

mongod reads a date string with timelib (PHP's `strtotime` scanner). The Rust
server ports the parts of that grammar clients use. Measured on 8.2.11,
2026-10-06, this found two SILENT wrong answers -- every 12-hour time
(`10:00 PM` was 01:00, `12:00 AM` the day before) and every zone abbreviation
(`UTC` twelve hours out, `GMT` / `EST` / `PST` likewise) -- because a trailing
military-zone rule read `PM` and the last letter of `UTC` as zone letters.

Since the timelib port (`crates/secantus-core/src/timelib`) the comparison is
EXACT: values and the full error text, through `$toDate` and `$dateFromString`,
plus `$dateFromString`'s `timezone` and `onError` cases (`TZ_CASES`).

    PROBE_MONGOD="mongodb://127.0.0.1:27095/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27096/?directConnection=true" \\
        python tools/probes/date_string_parsing.py
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson.codec_options import CodecOptions, DatetimeConversion

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")
CO = CodecOptions(datetime_conversion=DatetimeConversion.DATETIME_AUTO)

STRINGS = [
    "abc",
    "x",
    "",
    " ",
    "2024",
    "2024-13-01",
    "2024-02-30",
    "2024-01-01T25:00",
    "2024-01-01Z",
    "hello world",
    "12:00",
    "2024/01/02",
    "2024-01-01 junk",
    "1st Jan",
    "Jan 1 2024",
    "2024-01-01T00:00:00+25:00",
    "zz",
    "a1",
    "1a",
    "--",
    "2024-01-01T",
    "@@@",
    "abc def ghi",
    "Q",
    "01/02/2024",
    "Monday",
    "tomorrow",
    "2024-W01-1",
    "20240102",
    "20240102T030405",
    "2024-01-01T10:20:30",
    "2024-01-01T10:20:30Z",
    "2024-01-01T10:20:30.123Z",
    "2024-01-01T10:20:30+02:00",
    "2024-01-01T10:20:30-0530",
    "2024-01-01 10:20:30",
    "2024-01-01 10:20",
    "2024-01-01T10",
    "January 1 2024",
    "1 January 2024",
    "1 Jan 2024",
    "Jan 1, 2024",
    "2024 Jan 1",
    "Jan 2024",
    "1 Jan",
    "Jan 1",
    "January 2024",
    "Mon, 01 Jan 2024 10:00:00 GMT",
    "Mon Jan 1 10:00:00 2024",
    "2024-01-01 10:00 PM",
    "2024-01-01 10pm",
    "2024-01-01 12:00 AM",
    "2024-01-01 13:00 PM",
    "2024-01-01T10:00 PM",
    "2024-01-01 10:30 p.m.",
    "Jan 1 2024 10:00 PM",
    "10:00 PM 2024-01-01",
    "2024-01-01 10:00 PM +02:00",
    "2024-01-01 22:30:00 UTC",
    "2024-01-01 22:30:00 utc",
    "2024-01-01 22:30:00 GMT",
    "2024-01-01 22:30:00 EST",
    "2024-01-01 22:30:00 PST",
    "2024-01-01 22:30:00 CEST",
    "2024-01-01 22:30:00 IST",
    "2024-01-01 22:30:00 NDT",
    "2024-01-01 22:30:00 SGT",
    "2024-01-01 22:30:00 +02:00",
    "2024-01-01 22:30:00+02:00",
    "2024-01-01 22:30:00 +0200",
    "2024-01-01 22:30:00 -05",
    "Jan 1 2024 22:00:00",
    "Jan 1 2024 22:00",
    "22:00:00 2024-01-01",
    "22:00 2024-01-01",
    "2024-01-01 10:00 P",
    "2024-01-01 10:00 A",
    "2024-01-01 10:00Z",
    "2024-01-01 22:30:00 Europe/Dublin",
    "1/2/2024 10:00",
    "2024-1-2",
    "2024-01-01 24:00",
    "2024-01-01 23:59:60",
    "0000-01-01",
    "9999-12-31T23:59:59Z",
    "2024-01-01T00:00:00.5",
    "2024-06-15 08:00 MST",
    "15 June 2024 08:00 EDT",
    "2024-01-01 10:00PM",
    "2024-01-01 10 pm",
    "2024-01-01 10:00:00 am",
    "2024-01-01 12:00 PM",
    "2024-01-01 12 am",
    "2024-01-01 0:30 am",
    "2024-01-01 10:30 a.m.",
    "2024-01-01 10:30 P.M.",
    "2024-01-01 10:30pm Z",
    "2024-01-01 10:30 pm UTC",
    "10:00 PM",
    "2024-01-01 10:00:30.5 pm",
    "2024-01-01 9:05:07 PM",
    "2024-01-01 11:59:59 pm",
    "2024-01-01 22:30 UTC",
    "2024-01-01T22:30:00 UTC",
    "2024-01-01 22:30:00 CET",
    "2024-01-01 22:30:00 Z",
    "2024-01-01 22:30:00 America/New_York",
    "Tue, 01 Jan 2024 10:00:00 GMT",
    "Sun, 01 Jan 2024",
    "Fri 05 Jan 2024",
    "Tue Jan 1 10:00:00 2024",
    "Monday, January 1, 2024",
    "Feb 2024",
    "2024 Jan",
    "Jan, 2024",
    "Sept 2024",
    "Mon, 01 Jan 2024 10:00:00 +0000",
    "01 Jan 2024 10:00:00 GMT",
    "Jan 1 10:00:00 2024",
    "2024-01-01T23:59:60",
    "2024-01-01T23:59:60Z",
    "Wed 2024-01-01",
    "sat, 06 jan 2024",
    "-0001-01-01",
    "@-99999999999999",
    "@99999999999999",
    "9999-12-31",
    "+10000-01-01",
    "-100000-01-01",
    "@-62167219200",
    "@253402300800",
    "@1700000000.5",
    "tomorrow 2024-01-01",
    "2024-01-01 +1 day",
    "first monday of january 2024",
    "last day of 2024-02",
    "2024-01-31 +1 month",
    "yesterday noon 2024-03-01",
    "2024-01-01 next friday",
    "2024-01-01 3 weekdays",
    "10/Oct/2000:13:55:36 -0700",
    "2024.032",
    "2024-01-01T10:00:00.123456789Z",
    "back of 7pm 2024-01-01",
    "2024-01-01 12:00 (EST)",
    "2024-01-01 12:00 GMT+2",
]


#: `$dateFromString` with a `timezone` argument (and `onError`), measured the
#: same way: (dateString, timezone, onError or None).
TZ_CASES = [
    ("2024-01-01 10:00", "UTC", None),
    ("2024-01-01 10:00", "GMT", None),
    ("2024-01-01 10:00", "+00:00", None),
    ("2024-01-01 10:00", "+02:00", None),
    ("2024-01-01 10:00", "-0530", None),
    ("2024-01-01 10:00", "Europe/Dublin", None),
    ("2024-07-01 10:00", "Europe/Dublin", None),
    ("2024-01-01 10:00 EST", "UTC", None),
    ("2024-01-01 10:00 EST", "GMT", None),
    ("2024-01-01 10:00 EST", "+00:00", None),
    ("2024-01-01 10:00 EST", "+02:00", None),
    ("2024-01-01 10:00 EST", "America/New_York", None),
    ("2024-01-01 10:00 +05:00", "UTC", None),
    ("2024-01-01 10:00 +05:00", "+02:00", None),
    ("2024-01-01T10:00:00Z", "+02:00", None),
    ("@1700000000", "+02:00", None),
    ("abc", "+02:00", None),
    ("abc", "Not/AZone", None),
    ("2024-03-31 01:30", "Europe/Dublin", None),
    ("2024-03-31 02:30", "Europe/London", None),
    ("2024-10-27 01:30", "Europe/London", None),
    ("2024-03-10 02:30", "America/New_York", None),
    ("2024-11-03 01:30", "America/New_York", None),
    ("abc", "UTC", "fallback"),
    ("2024-01-01 10:00 EST", "+02:00", "fallback"),
    ("x", None, "fallback"),
]


def measure(uri: str) -> list[tuple]:
    client = pymongo.MongoClient(uri)
    db = client.date_parse_probe
    db.c.drop()
    db.c.insert_one({"_id": 1})
    out = []
    for s in STRINGS:
        for expr in ({"$toDate": s}, {"$dateFromString": {"dateString": s}}):
            out.append(run(db, expr))
    for s, tz, on_error in TZ_CASES:
        spec = {"dateString": s}
        if tz is not None:
            spec["timezone"] = tz
        if on_error is not None:
            spec["onError"] = on_error
        out.append(run(db, {"$dateFromString": spec}))
    client.drop_database("date_parse_probe")
    client.close()
    return out


def run(db, expr) -> tuple:
    cmd = {"aggregate": "c", "pipeline": [{"$project": {"r": expr}}], "cursor": {}}
    try:
        r = db.command(cmd, codec_options=CO)["cursor"]["firstBatch"][0]["r"]
        return ("OK", str(r))
    except pymongo.errors.OperationFailure as e:
        return (e.code, e.details["errmsg"].split(":: caused by :: ")[-1])


def labels() -> list[str]:
    out = []
    for s in STRINGS:
        out += [f"$toDate {s!r}", f"$dateFromString {s!r}"]
    out += [f"$dateFromString {s!r} tz={tz!r} onError={oe!r}" for s, tz, oe in TZ_CASES]
    return out


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    want, got = measure(MONGOD), measure(SERVER)
    # Self-check: the reference must read a 12-hour time as one.
    pm = 2 * STRINGS.index("2024-01-01 10:00 PM")
    if want[pm] != ("OK", "2024-01-01 22:00:00"):
        print(f"SELF-CHECK FAILED: mongod read '10:00 PM' as {want[pm]}")
        return 2
    bad = 0
    for label, w, g in zip(labels(), want, got, strict=True):
        if w != g:
            bad += 1
            print(f"DIFF  {label}\n   mongod {w}\n   ours   {g}")
    print(f"=== date strings: {bad} of {len(want)} divergent (values AND error text) ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
