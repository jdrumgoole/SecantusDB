"""Every time-zone name, both directions, on SecantusDB and on mongod.

For each zone name the system's tz database knows (plus a few mongod knows
and it may not), asks both servers:

- **instant to local**: `$dateToString` `%z` at 58 instants, January and July
  of 29 years from 1850 to 9000 -- history, the present and the far future;
- **local to instant**: `$dateFromParts` at 66 wall clocks, including the
  hours a daylight-saving change repeats or skips, in eleven years;
- `$dateToParts` and `%Z %H:%M:%S` in 1850, where local mean time has offsets
  in seconds.

and prints every answer that differs, error codes included. mongod answers
from timelib's built-in timezonedb, which the Rust server carries too
(`crates/secantus-core/tzdata/`, RM-249).

    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/time_zones.py

``PROBE_MONGOD`` defaults to ``mongodb://127.0.0.1:27041``.
"""

from __future__ import annotations

import datetime
import os
import sys
import zoneinfo
from typing import Any

from pymongo import MongoClient

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")

YEARS = [1850, 1890, 1900, 1910, 1920, 1930, 1940, 1945, 1950, 1960, 1970, 1980, 1990, 2000, 2010]
YEARS += [2020, 2024, 2025, 2026, 2030, 2040, 2060, 2087, 2099, 2100, 2101, 2150, 2500, 9000]
INSTANTS = [datetime.datetime(y, m, 15, 12) for y in YEARS for m in (1, 7)]
WALL_YEARS = (1850, 1900, 1918, 1940, 1945, 1970, 1990, 2026, 2100, 2150, 2500)
WALL_TIMES = (
    (1, 15, 12, 0),
    (3, 29, 1, 30),
    (4, 5, 2, 30),
    (7, 15, 0, 0),
    (10, 25, 1, 30),
    (11, 1, 1, 30),
)
WALLS = [(y, *t) for y in WALL_YEARS for t in WALL_TIMES]
EXTRA_NAMES = {"Factory", "America/Coyhaique", "US/Pacific-New", "factory", "america/new_york"}


def answers(uri: str, name: str) -> dict[str, Any]:
    """One server's answers for zone `name`, or the error for the lot."""
    coll = MongoClient(uri)["tzprobe"]["one"]
    if coll.estimated_document_count() != 1:
        coll.drop()
        coll.insert_one({"_id": 1})
    proj: dict[str, Any] = {
        f"z{d:%Y-%m}": {"$dateToString": {"date": d, "format": "%z", "timezone": name}}
        for d in INSTANTS
    }
    for y, m, d, h, mi in WALLS:
        proj[f"p{y}-{m:02}-{d:02}T{h:02}:{mi:02}"] = {
            "$dateFromParts": {
                "year": y,
                "month": m,
                "day": d,
                "hour": h,
                "minute": mi,
                "timezone": name,
            }
        }
    proj["parts1850"] = {
        "$dateToParts": {"date": datetime.datetime(1850, 1, 15, 12, 0, 30), "timezone": name}
    }
    proj["Z1850"] = {
        "$dateToString": {
            "date": datetime.datetime(1850, 1, 15, 12),
            "format": "%Z %H:%M:%S",
            "timezone": name,
        }
    }
    try:
        doc = next(coll.aggregate([{"$project": {"_id": 0, **proj}}]))
    except Exception as exc:  # noqa: BLE001 -- the error IS the answer
        return {"*": f"error {getattr(exc, 'code', None)}"}
    return {k: str(v) for k, v in doc.items()}


def main() -> int:
    if not SERVER:
        print("set PROBE_SERVER to the SecantusDB server's URI", file=sys.stderr)
        return 2
    names = sorted(zoneinfo.available_timezones() | EXTRA_NAMES)
    cells = differing = 0
    for name in names:
        ours, theirs = answers(SERVER, name), answers(MONGOD, name)
        for key in sorted(set(ours) | set(theirs)):
            cells += 1
            if ours.get(key) != theirs.get(key):
                differing += 1
                print(f"{name} {key}: mongod {theirs.get(key)}, ours {ours.get(key)}")
    print(f"  zones: {len(names)}   answers: {cells}   differing: {differing}")
    return 1 if differing else 0


if __name__ == "__main__":
    sys.exit(main())
