"""`$bucketAuto` with a `granularity`, across numeric TYPES, against mongod.

mongod rounds each boundary in the type of the value being rounded: a decimal
through a Decimal128 multiplier (with a quantum of its own -- `16.0000000000000`,
`1600.0000000000000`), everything else through the double path. The Rust server
refused the whole stage for any decimal `groupBy` until 2026-09-30.

Compares ``PROBE_SERVER`` with mongod, including field order and BSON types.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27055/?directConnection=true" \\
        python tools/probes/bucket_auto_granularity.py
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson import Decimal128 as D
from bson import Int64

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041/?directConnection=true")
SERVER = os.environ.get("PROBE_SERVER")

GRANULARITIES = [
    "R5",
    "R10",
    "R20",
    "R40",
    "R80",
    "1-2-5",
    "E6",
    "E12",
    "E24",
    "E48",
    "E96",
    "E192",
    "POWERSOF2",
]

VALUE_SETS = [
    [D("1.5"), D("20"), D("300"), D("4000")],
    [D("0.02"), D("0.7"), D("7"), D("3E+10")],
    [D("0.0000002"), D("0.8"), D("1.00"), D("64")],
    [2, D("20"), 300],
    [2.0, D("20"), Int64(300), 0.5],
    [D("0"), D("1"), D("2.50")],
    [1, 3, 17, 900],
    [0.3, 2.5, 99.9, 1024.0],
]


def run(coll, values, gran, buckets):
    coll.drop()
    coll.insert_many([{"_id": i, "x": v} for i, v in enumerate(values)])
    spec = {"groupBy": "$x", "buckets": buckets, "granularity": gran}
    try:
        rows = list(coll.aggregate([{"$bucketAuto": spec}]))
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code)
    return [
        [(k, type(v).__name__, str(v)) for k, v in row["_id"].items()] + [("count", row["count"])]
        for row in rows
    ]


def main() -> int:
    if not SERVER:
        print("PROBE_SERVER is required: this probe compares a running server with mongod")
        return 2
    mongod = pymongo.MongoClient(MONGOD).probe_bucket_gran.c
    ours = pymongo.MongoClient(SERVER).probe_bucket_gran.c
    total = bad = 0
    for values in VALUE_SETS:
        for gran in GRANULARITIES:
            for buckets in (1, 2, 3):
                total += 1
                want, got = run(mongod, values, gran, buckets), run(ours, values, gran, buckets)
                if want != got:
                    bad += 1
                    if bad <= 12:
                        print(f"DIFF {gran} buckets={buckets} {values}")
                        print(f"  mongod: {want}\n  ours:   {got}")
    for c in (mongod, ours):
        c.database.client.drop_database("probe_bucket_gran")
    print(f"=== $bucketAuto granularity: {bad} of {total} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
