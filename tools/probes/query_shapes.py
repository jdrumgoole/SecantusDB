"""Hand-built query filters against mongod, for the matcher code no test runs.

A line-coverage run of the Rust MongoDB server on 2026-10-21
(`docs-rust/coverage.md`) found 478 lines of `secantus-core/src/query.rs`
that the test suites never executed: most of `$jsonSchema`, the argument
checks of `$type`, `$mod`, `$size` and the bit operators, `$where`, and
corners of equality and `$expr`. These cases were written against those lines.

Each case is ``(label, filter)`` or ``(label, filter, documents)``. The filter
runs as a `find` (sorted by `_id`) over the documents (``DOCS`` when none are
given) on both servers, and only the disagreements are printed: the matched
`_id`s, or the error's code and message.

    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/query_shapes.py

``PROBE_MONGOD`` defaults to ``mongodb://127.0.0.1:27041``; ``PROBE_ONLY``
keeps the cases whose label contains it; ``--cases FILE`` adds the ``CASES``
of another file; ``--match`` also runs each case that agreed as an
aggregation ``$match`` after a stage (so it is not pushed into a `find`) and
compares that too; ``PROBE_DB`` names the database (default
``queryshapes``).
"""

from __future__ import annotations

import datetime
import os
import runpy
import sys

import pymongo
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp
from query_shapes_operators import CASES as _OPERATOR_CASES
from query_shapes_schema import CASES as _SCHEMA_CASES

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY")
DB = os.environ.get("PROBE_DB", "queryshapes")

WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5, 678000)

#: The default documents: one of each kind a matcher distinguishes.
DOCS: list[dict] = [
    {"_id": 1, "a": 1},
    {"_id": 2, "a": 2.5},
    {"_id": 3, "a": Int64(3)},
    {"_id": 4, "a": Decimal128("4.0")},
    {"_id": 5, "a": float("nan")},
    {"_id": 6, "a": -7},
    {"_id": 7, "a": "abc"},
    {"_id": 8, "a": "ABCé"},
    {"_id": 9, "a": [1, 2, 3]},
    {"_id": 10, "a": [[1, 2], 3]},
    {"_id": 11, "a": []},
    {"_id": 12, "a": {"b": 1, "c": [1, 2]}},
    {"_id": 13, "a": [{"b": 1}, {"b": 2, "c": 3}]},
    {"_id": 14, "a": None},
    {"_id": 15},
    {"_id": 16, "a": True},
    {"_id": 17, "a": WHEN},
    {"_id": 18, "a": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")},
    {"_id": 19, "a": Binary(b"\x05\x00", 0)},
    {"_id": 20, "a": Regex("^a", "i")},
    {"_id": 21, "a": Timestamp(1, 2)},
    {"_id": 22, "a": MinKey()},
    {"_id": 23, "a": MaxKey()},
    {"_id": 24, "a": {"b": {"c": 1}}, "b": 5},
    {"_id": 25, "a": 0, "b": "x", "c": [1.5, "y", None]},
]

#: The cases, in two files: `$jsonSchema`; and every other operator.
CASES: list[tuple] = _OPERATOR_CASES + _SCHEMA_CASES

_CLIENT_OPTS = dict(
    directConnection=True,
    serverSelectionTimeoutMS=8000,
    datetime_conversion="DATETIME_AUTO",
)


def _outcome(fn) -> tuple:
    try:
        return ("OK", fn())
    except pymongo.errors.PyMongoError as e:
        d = getattr(e, "details", None)
        msg = d.get("errmsg", "") if isinstance(d, dict) else str(e)
        return (getattr(e, "code", None), msg[:220])
    except (TypeError, ValueError, OverflowError, RecursionError) as e:
        return ("CLIENT", type(e).__name__)


def run(cli, filt, docs, in_match: bool = False) -> tuple:
    coll = cli[DB].c
    coll.drop()
    if docs:
        coll.insert_many(docs)
    if in_match:
        pipeline = [{"$addFields": {"__probe": 1}}, {"$match": filt}, {"$sort": {"_id": 1}}]
        return _outcome(lambda: [d["_id"] for d in coll.aggregate(pipeline)])
    return _outcome(lambda: [d["_id"] for d in coll.find(filt).sort("_id", 1)])


def main() -> int:
    if not SERVER:
        sys.exit("set PROBE_SERVER to the Rust server's URI")
    cases = list(CASES)
    if "--cases" in sys.argv:
        cases += runpy.run_path(sys.argv[sys.argv.index("--cases") + 1])["CASES"]
    sec = pymongo.MongoClient(SERVER, **_CLIENT_OPTS)
    mon = pymongo.MongoClient(MONGOD, **_CLIENT_OPTS)
    print(f"  server under test: {SERVER}; mongod {mon.server_info()['version']}")
    wrong, code, msg, total = [], [], [], 0
    for case in cases:
        label, filt = case[0], case[1]
        docs = case[2] if len(case) > 2 else DOCS
        if ONLY and ONLY not in label:
            continue
        total += 1
        m, o = run(mon, filt, docs), run(sec, filt, docs)
        if m == o and "--match" in sys.argv:
            label = f"[$match] {label}"
            m, o = run(mon, filt, docs, True), run(sec, filt, docs, True)
        if m == o:
            continue
        entry = (label, filt, m, o)
        if m[0] == "OK" and o[0] == "OK":
            wrong.append(entry)
        elif m[0] != o[0]:
            code.append(entry)
        else:
            msg.append(entry)
    print(
        f"  cases: {total}   WRONG RESULT: {len(wrong)}   "
        f"different code: {len(code)}   message only: {len(msg)}"
    )
    for title, group in (("WRONG RESULT", wrong), ("DIFFERENT CODE", code), ("MESSAGE ONLY", msg)):
        if not group:
            continue
        print(f"\n  === {title}: {len(group)} ===")
        for label, filt, m, o in group:
            print(
                f"    {label}\n        filter {filt!r}\n        mongod {m!r}\n        ours   {o!r}"
            )
    return 1 if (wrong or code or msg) else 0


if __name__ == "__main__":
    sys.exit(main())
