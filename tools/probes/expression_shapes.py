"""Hand-built expression shapes against mongod, for what the generic sweep cannot build.

`agg_expressions.py` feeds every operator the same corpus of values in one- and
two-argument form. That reaches an operator's type checks, but not the branches
behind a STRUCTURED argument: `$switch`'s branches, `$sortArray`'s `sortBy`,
`$dateDiff`'s units and `startOfWeek`, `$round`'s place, `$convert`'s formats,
`$substrCP`'s bounds. A line-coverage run of the Rust MongoDB server on
2026-10-21 (`docs-rust/coverage.md`) found ~1,200 lines of
`secantus-core/src/expressions.rs` that neither the test suites nor that sweep
ever executed, `op_switch` among them. These cases were written against those
lines.

Each case is ``(label, expression)`` or ``(label, expression, document)``; the
expression is evaluated with ``$addFields`` over the document (``DOC`` when none
is given), on both servers, and only the disagreements are printed. Values are
compared by ``repr``, which keeps the BSON type: ``2``, ``Int64(2)``, ``2.0`` and
``Decimal128('2')`` all differ.

The Rust server only (the Python servers are deprecated)::

    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/expression_shapes.py

``PROBE_MONGOD`` defaults to ``mongodb://127.0.0.1:27041``; ``PROBE_ONLY``
keeps the cases whose label contains it; ``--cases FILE`` adds the ``CASES`` of
another file, for iterating on new ones before they move in here; ``--match`` also
runs each case that agreed inside ``$match: {$expr}`` and compares the outcome (an
error's code, or success); ``PROBE_DB``
names the database (default ``exprshapes``), so two runs can share the servers;
``--sbe`` evaluates each case in a ``$group`` mongod runs in its slot-based engine
(``[{$group: {_id: null, z: {$first: <expr>}}}]`` over the document stored in the
collection), where SBE's answers differ from the classic engine's (RM-280).
"""

from __future__ import annotations

import datetime
import os
import runpy
import sys

import bson.errors
import pymongo
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp
from expression_shapes_dates import CASES as _DATE_CASES
from expression_shapes_operators import CASES as _OPERATOR_CASES

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY")
#: The database the probe uses on both servers; give concurrent runs their own.
DB = os.environ.get("PROBE_DB", "exprshapes")

WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5, 678000)

#: The default document. Short names keep the labels readable.
DOC = {
    "_id": 1,
    "n": 2,
    "d": 1.5,
    "s": "abc",
    "u": "héllo wörld",
    "arr": [3, 1, 2],
    "docs": [{"a": 2, "b": "x"}, {"a": 1, "b": "y"}, {"a": 3}],
    "o": {"k": 1, "j": {"x": 1}},
    "t": WHEN,
    "t2": datetime.datetime(2027, 3, 4, 5, 6, 7),
    "oid": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"),
    "dec": Decimal128("2.5"),
    "big": Int64(2**40),
    "bin": Binary(b"\x01\x02\x03\x04", 0),
    "ts": Timestamp(1, 1),
    "re": Regex("a", "i"),
    "lo": MinKey(),
    "hi": MaxKey(),
    "nul": None,
}

#: The shapes, in two files: arrays, strings, objects, conversion, regex and
#: equality; and dates and arithmetic.
CASES: list[tuple] = _OPERATOR_CASES + _DATE_CASES

_CLIENT_OPTS = dict(
    directConnection=True,
    serverSelectionTimeoutMS=8000,
    datetime_conversion="DATETIME_AUTO",
)


#: A value no expression here produces, so the `$match` context filters nothing in.
_NEVER = "\x00never"


def run(cli, expr, doc, in_match: bool = False) -> tuple:
    if "--sbe" in sys.argv:
        return run_sbe(cli, expr, doc)
    if in_match:
        last = {"$match": {"$expr": {"$eq": [expr, _NEVER]}}}
    else:
        last = {"$addFields": {"z": expr}}
    pipeline = [{"$limit": 1}, {"$replaceWith": {"$literal": doc}}, last]
    try:
        out = list(cli[DB].c.aggregate(pipeline))
        if in_match:
            return ("OK", None)
        if not out:
            return ("EMPTY", None)
        return ("OK", "MISSING" if "z" not in out[0] else repr(out[0]["z"]))
    except pymongo.errors.PyMongoError as e:
        d = getattr(e, "details", None)
        msg = d.get("errmsg", "") if isinstance(d, dict) else str(e)
        return (getattr(e, "code", None), msg[:200])
    except (TypeError, ValueError, OverflowError, RecursionError) as e:
        return ("CLIENT", type(e).__name__)


def run_sbe(cli, expr, doc) -> tuple:
    """`expr` in a `$group` that leads the pipeline, which mongod pushes into SBE
    when every operator in it is one SBE compiles."""
    coll = cli[DB].sbe
    coll.drop()
    coll.insert_one(doc)
    try:
        out = list(coll.aggregate([{"$group": {"_id": None, "z": {"$first": expr}}}]))
        return ("OK", repr(out[0].get("z", "MISSING")))
    except pymongo.errors.PyMongoError as e:
        d = getattr(e, "details", None)
        msg = d.get("errmsg", "") if isinstance(d, dict) else str(e)
        return (getattr(e, "code", None), msg[:200])
    except bson.errors.InvalidBSON:
        # A reply the driver cannot decode, such as a string that is not UTF-8.
        return ("INVALID BSON", None)
    except (TypeError, ValueError, OverflowError, RecursionError) as e:
        return ("CLIENT", type(e).__name__)


def main() -> int:
    if not SERVER:
        sys.exit("set PROBE_SERVER to the Rust server's URI")
    cases = list(CASES)
    if "--cases" in sys.argv:
        cases += runpy.run_path(sys.argv[sys.argv.index("--cases") + 1])["CASES"]
    sec = pymongo.MongoClient(SERVER, **_CLIENT_OPTS)
    mon = pymongo.MongoClient(MONGOD, **_CLIENT_OPTS)
    print(f"  server under test: {SERVER}; mongod {mon.server_info()['version']}")
    for cli in (mon, sec):
        cli[DB].c.drop()
        cli[DB].c.insert_one({"_id": 0})

    value, code, msg, total = [], [], [], 0
    for case in cases:
        label, expr = case[0], case[1]
        doc = case[2] if len(case) > 2 else DOC
        if ONLY and ONLY not in label:
            continue
        total += 1
        m, o = run(mon, expr, doc), run(sec, expr, doc)
        if m == o and "--match" in sys.argv:
            # The same expression under `$match: {$expr}`, outcome only: an
            # error that `$addFields` reports correctly can still surface there
            # as "not supported", which the first context cannot see.
            label = f"[$match] {label}"
            m, o = run(mon, expr, doc, True), run(sec, expr, doc, True)
            m, o = m[:1], o[:1]
        if m == o:
            continue
        if m[0] == "OK" and o[0] == "OK":
            if "$rand" in label:  # two correct servers draw different numbers
                continue
            value.append((label, m, o))
        elif m[0] != o[0]:
            code.append((label, m, o))
        else:
            msg.append((label, m, o))
    print(
        f"  cases: {total}   WRONG VALUE: {len(value)}   "
        f"different code: {len(code)}   message only: {len(msg)}\n"
    )
    for title, rows in (
        ("WRONG VALUE (both succeeded, answers differ)", value),
        ("DIFFERENT CODE / one errored", code),
        ("MESSAGE ONLY (code matches)", msg),
    ):
        if rows:
            print(f"  === {title}: {len(rows)} ===")
            for label, m, o in rows:
                print(f"    {label}\n        mongod {m}\n        ours   {o}")
            print()
    return 1 if (value or code or msg) else 0


if __name__ == "__main__":
    sys.exit(main())
