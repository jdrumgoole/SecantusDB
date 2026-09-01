"""The date-operator family crossed with timezones, units and bin sizes.

The backlog carried this as "`$dateFromString` with a named IANA timezone
(Rust server)... needs a timezone database -- a dependency decision, not an
afternoon's port". Measuring it found all three parts of that wrong:

* `chrono-tz` is ALREADY a dependency and the IANA database is already bundled,
  which is why `$hour`/`$dayOfWeek`/`$dateToString` with a named zone work on
  the Rust server today. What is missing is the wall-clock -> instant direction.
* It is `$dateFromParts` as well as `$dateFromString`.
* Two defects the entry never mentioned, both on BOTH servers -- and the first
  is a silent WRONG ANSWER, not an error:
    - `$dateTrunc` ignored `timezone` entirely, truncating in UTC. mongod
      truncates the wall clock IN the zone and returns that instant, so a
      "day" bucket for America/New_York starts at 04:00Z, not 00:00Z. Every
      timezone-aware daily rollup was silently bucketed on the wrong boundary.
    - an unrecognised / empty zone name is `40485` on mongod, raised while
      OPTIMISING the pipeline when the name is a constant.

Run against BOTH servers -- separate implementations, and the parity suites are
satisfied by both being wrong together.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/date_timezones.py
    PROBE_SERVER="mongodb://127.0.0.1:27055" ...   # adds the Rust column
"""

import datetime as dt
import os
import sys
import tempfile

import pymongo

from secantus import SecantusDBServer

targets = [
    ("mongod", pymongo.MongoClient(os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")))
]
_s = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
_s.start()
targets.append(("python", pymongo.MongoClient(_s.uri)))
if os.environ.get("PROBE_SERVER"):
    targets.append(("rust", pymongo.MongoClient(os.environ["PROBE_SERVER"])))

# A summer instant (so northern DST is active) with sub-second precision, and a
# winter one for $dateDiff. Chatham is +12:45 -- a quarter-hour zone, which is
# what catches an implementation that only handles whole-hour offsets.
D = dt.datetime(2026, 7, 4, 15, 30, 45, 123000, tzinfo=dt.timezone.utc)
W = dt.datetime(2026, 1, 4, 15, 30, 45, tzinfo=dt.timezone.utc)
# The day BEFORE the US spring-forward, at noon Eastern -- so a +1 day shift
# crosses the transition and the wall-clock answer differs from the 24h one.
DST = dt.datetime(2026, 3, 7, 17, 0, tzinfo=dt.timezone.utc)

ZONES = [
    "UTC",
    "America/New_York",
    "Europe/London",
    "Asia/Kolkata",
    "Australia/Lord_Howe",
    "Pacific/Chatham",
    "America/Sao_Paulo",
    "Etc/GMT+5",
    "+05:30",
    "-08:00",
    "Not/AZone",
    "America/new_york",
    "",
]
UNITS = ["year", "quarter", "month", "week", "day", "hour", "minute", "second", "millisecond"]

# Endpoint pairs for `$dateDiff`, stored as real BSON dates on the document
# rather than built with `$dateFromString` -- otherwise a server that cannot
# parse the string form fails every shape and hides what is being measured.
DIFF_PAIRS = [
    (dt.datetime(2026, 7, 4, 2), dt.datetime(2026, 7, 4, 23)),
    (dt.datetime(2026, 7, 4, 23), dt.datetime(2026, 7, 4, 2)),
    (dt.datetime(2025, 12, 31, 23), dt.datetime(2026, 1, 1, 2)),
    (dt.datetime(2026, 7, 1, 2), dt.datetime(2026, 7, 31, 23)),
    (dt.datetime(2026, 1, 1, 2), dt.datetime(2026, 1, 1, 23)),
    (dt.datetime(2026, 7, 5, 2), dt.datetime(2026, 7, 5, 23)),
    (dt.datetime(2026, 2, 28, 12), dt.datetime(2026, 3, 1, 12)),
]


def shapes():
    for tz in ZONES:
        yield (
            f"$dateFromString {tz}",
            {"$dateFromString": {"dateString": "2026-07-04T15:30:45", "timezone": tz}},
        )
        yield (
            f"$dateToString {tz}",
            {"$dateToString": {"date": "$d", "format": "%Y-%m-%dT%H:%M:%S", "timezone": tz}},
        )
        yield (f"$hour {tz}", {"$hour": {"date": "$d", "timezone": tz}})
        yield (f"$dayOfWeek {tz}", {"$dayOfWeek": {"date": "$d", "timezone": tz}})
        yield (f"$dateToParts {tz}", {"$dateToParts": {"date": "$d", "timezone": tz}})
        yield (
            f"$dateFromParts {tz}",
            {
                "$dateFromParts": {
                    "year": 2026,
                    "month": 7,
                    "day": 4,
                    "hour": 15,
                    "minute": 30,
                    "timezone": tz,
                }
            },
        )
        yield (
            f"$dateAdd {tz}",
            {"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 1, "timezone": tz}},
        )
        yield (
            f"$dateDiff {tz}",
            {
                "$dateDiff": {
                    "startDate": "$w",
                    "endDate": "$d",
                    "unit": "day",
                    "timezone": tz,
                }
            },
        )
        for unit in UNITS:
            yield (
                f"$dateTrunc {unit} {tz}",
                {"$dateTrunc": {"date": "$d", "unit": unit, "timezone": tz}},
            )

    # `$dateDiff` counts BOUNDARY CROSSINGS, and the boundaries are local -- so
    # two instants inside one UTC day can be one local day apart.
    for i, _pair in enumerate(DIFF_PAIRS):
        for tz in ["UTC", "America/New_York", "Asia/Kolkata"]:
            for unit in ["year", "quarter", "month", "week", "day", "hour"]:
                yield (
                    f"$dateDiff {unit} {tz} pair{i}",
                    {
                        "$dateDiff": {
                            "startDate": f"$p{i}a",
                            "endDate": f"$p{i}b",
                            "unit": unit,
                            "timezone": tz,
                        }
                    },
                )

    # `$dateAdd` / `$dateSubtract` of a CALENDAR unit keeps the local wall
    # clock, so +1 day across a spring-forward is 23 real hours, not 24.
    for tz in ["UTC", "America/New_York", "Australia/Lord_Howe"]:
        for unit, amount in [("day", 1), ("day", -1), ("week", 1), ("month", 1), ("hour", 24)]:
            for op in ["$dateAdd", "$dateSubtract"]:
                yield (
                    f"{op} {unit}{amount:+d} {tz} dst",
                    {
                        op: {
                            "startDate": "$dst",
                            "unit": unit,
                            "amount": amount,
                            "timezone": tz,
                        }
                    },
                )

    # binSize and startOfWeek interact with the zone shift.
    for tz in ["UTC", "America/New_York", "Asia/Kolkata", "Pacific/Chatham"]:
        for unit, extra in [
            ("hour", {"binSize": 5}),
            ("day", {"binSize": 3}),
            ("minute", {"binSize": 45}),
            ("month", {"binSize": 4}),
            ("year", {"binSize": 2}),
            ("week", {"startOfWeek": "monday"}),
            ("week", {"startOfWeek": "friday"}),
            ("week", {}),
        ]:
            label = f"$dateTrunc {unit} {tz} {extra}"
            yield (label, {"$dateTrunc": {"date": "$d", "unit": unit, "timezone": tz, **extra}})


def run(cli, expr):
    c = cli["dtz"]["c"]
    try:
        c.drop()
        doc = {"_id": 1, "d": D, "w": W, "dst": DST}
        for i, (a, b) in enumerate(DIFF_PAIRS):
            doc[f"p{i}a"] = a.replace(tzinfo=dt.timezone.utc)
            doc[f"p{i}b"] = b.replace(tzinfo=dt.timezone.utc)
        c.insert_one(doc)
        out = list(c.aggregate([{"$project": {"r": expr}}]))
        return repr(out[0].get("r"))
    except Exception as e:  # noqa: BLE001 - the error IS the measurement
        return f"ERR {getattr(e, 'code', '?')}: {str(e).splitlines()[0][:90]}"


total = 0
divergent = 0
by_op: dict[str, int] = {}
for name, expr in shapes():
    total += 1
    res = {label: run(cli, expr) for label, cli in targets}
    if len(set(res.values())) > 1:
        divergent += 1
        op = name.split()[0]
        by_op[op] = by_op.get(op, 0) + 1
        print(f"  {name}")
        for k, v in res.items():
            print(f"      {k:8s} {v}")

print(f"\n=== {total} date/timezone shapes: {divergent} divergent ===")
for op, n in sorted(by_op.items(), key=lambda kv: -kv[1]):
    print(f"  {n:4d}  {op}")
sys.exit(1 if divergent else 0)
