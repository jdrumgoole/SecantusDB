"""Index and count ARGUMENTS across every numeric kind, against mongod.

The operators that take a position or a count -- `$slice`, `$substrCP`,
`$indexOfCP` / `$indexOfBytes` / `$indexOfArray`, `$arrayElemAt`, `$range` --
each check that the argument is a whole number that fits in 32 bits, with their
own code and their own rendering of the value. The Rust server skipped most of
those checks, and several were silent wrong answers rather than a wrong
message (measured 8.2.11, 2026-09-30):

* `$slice: [[1, 2, 3], NumberDecimal("1")]` returned null (mongod: `[1]`);
* `$slice: [[1, 2, 3], 3e9]` returned the whole array (mongod: 28726);
* `$slice: [[1, 2, 3], 0, 0]` returned `[]` (mongod: 28729, a count must be
  positive);
* `$indexOfCP: ["abc", "b", 3e9]` returned -1 (mongod: 40096).

The `$jsonSchema` block covers the other half of that sweep: `type: "integer"`
is refused while PARSING, and was accepted.

Compares ``PROBE_SERVER`` with mongod; there is no embedded-server column.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27055/?directConnection=true" \\
        python tools/probes/int32_arguments.py
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson import Decimal128, Int64

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041/?directConnection=true")
SERVER = os.environ.get("PROBE_SERVER")

#: Every numeric kind at the boundaries that matter: whole / fractional, inside
#: and just outside int32, non-positive, and each as int / long / double /
#: decimal.
VALUES = [
    1,
    Int64(1),
    1.0,
    Decimal128("1"),
    1.5,
    Decimal128("1.5"),
    0,
    -1,
    2147483647,
    2147483648.0,
    Int64(2147483648),
    -2147483649.0,
    Int64(3_000_000_000),
    3e9,
    Decimal128("3E+9"),
]

#: `(label, build)` -- `build(v)` places the value in one argument position.
SHAPES = [
    ("$slice n", lambda v: {"$slice": [[1, 2, 3], v]}),
    ("$slice pos", lambda v: {"$slice": [[1, 2, 3], v, 1]}),
    ("$slice count", lambda v: {"$slice": [[1, 2, 3], 0, v]}),
    ("$substrCP start", lambda v: {"$substrCP": ["abcdef", v, 2]}),
    ("$substrCP length", lambda v: {"$substrCP": ["abcdef", 1, v]}),
    ("$indexOfCP start", lambda v: {"$indexOfCP": ["abcb", "b", v]}),
    ("$indexOfCP end", lambda v: {"$indexOfCP": ["abcb", "b", 0, v]}),
    ("$indexOfBytes start", lambda v: {"$indexOfBytes": ["abcb", "b", v]}),
    ("$indexOfArray start", lambda v: {"$indexOfArray": [[1, 2, 1], 1, v]}),
    ("$indexOfArray end", lambda v: {"$indexOfArray": [[1, 2, 1], 1, 0, v]}),
    ("$arrayElemAt", lambda v: {"$arrayElemAt": [[1, 2, 3], v]}),
    ("$range end", lambda v: {"$range": [0, v]}),
]

SCHEMA_TYPES = [
    {"type": "integer"},
    {"bsonType": "integer"},
    {"type": "int"},
    {"type": "number"},
    {"bsonType": "int"},
    {"type": ["string", "integer"]},
    {"bsonType": ["int", "integer"]},
    {"type": "foo"},
    {"bsonType": "foo"},
]


def outcome(coll, expr=None, schema=None):
    try:
        if schema is not None:
            return ("n", len(list(coll.find({"$jsonSchema": {"properties": {"v": schema}}}))))
        doc = next(coll.aggregate([{"$project": {"_id": 0, "r": expr}}]))
        return ("ok", repr(doc.get("r")))
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code, (e.details or {}).get("errmsg"))


def main() -> int:
    if not SERVER:
        print("PROBE_SERVER is required: this probe compares a running server with mongod")
        return 2
    colls = []
    for uri in (MONGOD, SERVER):
        c = pymongo.MongoClient(uri).probe_int32.c
        c.drop()
        c.insert_one({"_id": 1, "v": 5})
        colls.append(c)
    mongod, ours = colls
    total = bad = 0
    for label, build in SHAPES:
        for v in VALUES:
            total += 1
            want, got = outcome(mongod, build(v)), outcome(ours, build(v))
            if want != got:
                bad += 1
                print(f"DIFF {label} {v!r}\n  mongod: {want}\n  ours:   {got}")
    for schema in SCHEMA_TYPES:
        total += 1
        want, got = outcome(mongod, schema=schema), outcome(ours, schema=schema)
        if want != got:
            bad += 1
            print(f"DIFF $jsonSchema {schema}\n  mongod: {want}\n  ours:   {got}")
    for c in colls:
        c.database.client.drop_database("probe_int32")
    print(f"=== 32-bit index / count arguments: {bad} of {total} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
