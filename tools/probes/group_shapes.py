"""Hand-built `$group` / `$bucket` / `$bucketAuto` / `$sortByCount` pipelines
against mongod, for the accumulator code no test runs.

A line-coverage run of the Rust MongoDB server on 2026-10-21
(`docs-rust/coverage.md`) found 455 lines of `secantus-core/src/group.rs`
that the test suites never executed. The cases in `group_shapes_cases.py` were
written against them.

Each case is ``(label, pipeline)`` or ``(label, pipeline, documents)``. The
pipeline runs over the documents (``DOCS`` when none are given) on both
servers, and only the disagreements are printed. Results are compared as
CANONICAL extended JSON, so a value of the wrong BSON type (an int where mongod
answers a long, a double where it answers a decimal) is a difference, not a
match. Group order is not part of the comparison unless the pipeline sorts:
each answer is sorted by its canonical form first.

    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/group_shapes.py

``PROBE_MONGOD`` defaults to ``mongodb://127.0.0.1:27041``; ``PROBE_ONLY``
keeps the cases whose label contains it; ``PROBE_DB`` names the database
(default ``groupshapes``).
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson import json_util
from bson.json_util import JSONMode, JSONOptions
from group_shapes_cases import CASES, DOCS

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY")
DB = os.environ.get("PROBE_DB", "groupshapes")

_CANON = JSONOptions(json_mode=JSONMode.CANONICAL)
_CLIENT_OPTS = dict(directConnection=True, serverSelectionTimeoutMS=8000)


def _sorts(pipeline: list) -> bool:
    """Whether the result's ORDER is defined by the pipeline."""
    return any(
        next(iter(stage)) in ("$sort", "$sortByCount", "$bucket", "$bucketAuto")
        for stage in pipeline
    )


def _canon(v) -> str:
    return json_util.dumps(v, json_options=_CANON)


def _unordered(pipeline: list) -> bool:
    """Whether the answer holds an order mongod does not fix. Its `$addToSet`
    and `$setUnion` come out of a hash table, and the groups in front of
    `$sortByCount`'s sort too: the same pipeline on the same mongod gave each
    of two orders, 9 and 11 times in 20 runs (8.2.11, 2026-10-23)."""
    text = repr(pipeline)
    return "$addToSet" in text or "$setUnion" in text or "$sortByCount" in text


def run(cli, pipeline: list, docs: list) -> tuple:
    coll = cli[DB].c
    coll.drop()
    if docs:
        coll.insert_many(docs)
    try:
        got = list(coll.aggregate(pipeline))
    except pymongo.errors.PyMongoError as e:
        d = getattr(e, "details", None)
        msg = d.get("errmsg", "") if isinstance(d, dict) else str(e)
        return (getattr(e, "code", None), msg[:300])
    if _unordered(pipeline):
        # Each array as the set it is, and equal counts in either order.
        for d in got:
            for k, v in d.items():
                if isinstance(v, list):
                    d[k] = sorted(v, key=_canon)
        if any("$sortByCount" in stage for stage in pipeline):
            got.sort(key=lambda d: (-d.get("count", 0), _canon(d)))
    out = [_canon(d) for d in got]
    return ("OK", out if _sorts(pipeline) else sorted(out))


def main() -> int:
    if not SERVER:
        sys.exit("set PROBE_SERVER to the Rust server's URI")
    sec = pymongo.MongoClient(SERVER, **_CLIENT_OPTS)
    mon = pymongo.MongoClient(MONGOD, **_CLIENT_OPTS)
    print(f"  server under test: {SERVER}; mongod {mon.server_info()['version']}")
    wrong, code, msg, total = [], [], [], 0
    for case in CASES:
        label, pipeline = case[0], case[1]
        docs = case[2] if len(case) > 2 else DOCS
        if ONLY and ONLY not in label:
            continue
        total += 1
        m, o = run(mon, pipeline, docs), run(sec, pipeline, docs)
        if m == o:
            continue
        entry = (label, pipeline, m, o)
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
        for label, pipeline, m, o in group:
            print(f"    {label}\n        pipeline {pipeline!r}")
            print(f"        mongod {m!r}\n        ours   {o!r}")
    for cli in (sec, mon):
        cli.drop_database(DB)
    return 1 if (wrong or code or msg) else 0


if __name__ == "__main__":
    sys.exit(main())
